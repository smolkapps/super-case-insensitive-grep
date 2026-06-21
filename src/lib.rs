//! Core logic for `scig` (super-case-insensitive-grep).
//!
//! The headline feature is [`expand_pattern`]: it turns an identifier pattern
//! such as `FooBar|first_name` into a single case-insensitive regex
//! (`(?i)foo[-_]?bar|first[-_]?name`) that matches the *same* identifier
//! written in any of the common naming conventions:
//!
//! * `camelCase`     — `fooBar`
//! * `PascalCase`    — `FooBar`
//! * `snake_case`    — `foo_bar`
//! * `kebab-case`    — `foo-bar`
//! * `CONSTANT_CASE` — `FOO_BAR`
//! * the squashed / lowercase form — `foobar`
//!
//! The crate also exposes a small native search engine ([`search_path`]) built
//! on the `regex` and `ignore` crates so the tool does not have to shell out to
//! `egrep`, plus a helper ([`expanded_egrep_command`]) for users who prefer to.

use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use regex::bytes::Regex as BytesRegex;

/// Errors that can arise while building or running a search.
#[derive(Debug)]
pub enum Error {
    /// The expanded pattern was not a valid regular expression.
    Regex(regex::Error),
    /// An I/O error occurred while walking or reading files.
    Io(io::Error),
    /// The caller supplied an empty pattern.
    EmptyPattern,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Regex(e) => write!(f, "invalid regular expression: {e}"),
            Error::Io(e) => write!(f, "i/o error: {e}"),
            Error::EmptyPattern => write!(f, "pattern must not be empty"),
        }
    }
}

impl std::error::Error for Error {}

impl From<regex::Error> for Error {
    fn from(e: regex::Error) -> Self {
        Error::Regex(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

/// Split an identifier into its lowercase word tokens, understanding every
/// common convention at once (and messy mixes of them).
///
/// Separators recognised: `_`, `-`, ` ` (space) and `.`.
/// Boundaries recognised within a run of characters:
///   * lower/digit -> upper            (`fooBar`  -> foo | bar)
///   * acronym tail (`UPPERlower`)     (`HTTPServer` -> http | server)
///   * letter <-> digit                (`utf8Mb4` -> utf | 8 | mb | 4)
///
/// ```
/// use scig::tokenize;
/// assert_eq!(tokenize("getHTTPResponseCode"),
///            vec!["get", "http", "response", "code"]);
/// assert_eq!(tokenize("first_name"), vec!["first", "name"]);
/// assert_eq!(tokenize("FOO_BAR"), vec!["foo", "bar"]);
/// assert_eq!(tokenize("parseUTF8"), vec!["parse", "utf", "8"]);
/// ```
pub fn tokenize(ident: &str) -> Vec<String> {
    let chars: Vec<char> = ident.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();

    let is_sep = |c: char| c == '_' || c == '-' || c == ' ' || c == '.';

    for (i, &c) in chars.iter().enumerate() {
        if is_sep(c) {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
            continue;
        }

        if !cur.is_empty() {
            let prev = chars[i - 1];
            let boundary = needs_split(prev, c, chars.get(i + 1).copied());
            if boundary {
                tokens.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Decide whether a boundary falls *before* `c`, given the previous char and
/// (for acronym detection) the char that follows `c`.
fn needs_split(prev: char, c: char, next: Option<char>) -> bool {
    let prev_lower = prev.is_lowercase();
    let prev_upper = prev.is_uppercase();
    let prev_digit = prev.is_ascii_digit();
    let c_upper = c.is_uppercase();
    let c_lower = c.is_lowercase();
    let c_digit = c.is_ascii_digit();

    // lower/digit -> upper : fooBar, utf8B
    if (prev_lower || prev_digit) && c_upper {
        return true;
    }
    // acronym tail: UPPER UPPER lower  => split before the last upper.
    // e.g. "HTTPServer": at 'S' we have prev='P'(upper), c='S'(upper),
    // next='e'(lower) -> split before 'S'.
    if prev_upper && c_upper && matches!(next, Some(n) if n.is_lowercase()) {
        return true;
    }
    // letter <-> digit transitions
    if (prev.is_alphabetic()) && c_digit {
        return true;
    }
    if prev_digit && c.is_alphabetic() {
        return true;
    }
    let _ = c_lower; // kept for readability of the rule set
    false
}

// ---------------------------------------------------------------------------
// Pattern expansion
// ---------------------------------------------------------------------------

/// Characters that, if present in an alternation branch, mean "this branch is
/// raw regex — pass it through untouched rather than treating it as an
/// identifier to expand".
fn looks_like_regex(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(
            c,
            '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|'
        )
    })
}

/// Split on top-level `|` only (we do not support nested groups in identifier
/// branches — those would contain `(` and be treated as raw regex anyway).
fn split_alternation(pattern: &str) -> Vec<&str> {
    pattern.split('|').collect()
}

/// Join already-lowercased tokens into the convention-agnostic body of a
/// regex branch, e.g. `["foo","bar"]` -> `foo[-_ ]?bar`.
fn join_tokens(tokens: &[String]) -> String {
    let escaped: Vec<String> = tokens.iter().map(|t| regex::escape(t)).collect();
    escaped.join("[-_ ]?")
}

/// Expand an identifier pattern into a single case-insensitive regex string.
///
/// * Each top-level `|`-separated branch that *looks like* a plain identifier
///   is tokenized and rejoined with an optional `[-_ ]?` between tokens.
/// * Branches that contain regex metacharacters are passed through verbatim so
///   you can freely mix raw regex with identifier expansion.
/// * The whole expression is made case-insensitive via a leading `(?i)`.
///
/// ```
/// use scig::expand_pattern;
/// assert_eq!(expand_pattern("FooBar").unwrap(), "(?i)foo[-_ ]?bar");
/// assert_eq!(expand_pattern("FooBar|first_name").unwrap(),
///            "(?i)foo[-_ ]?bar|first[-_ ]?name");
/// ```
pub fn expand_pattern(pattern: &str) -> Result<String, Error> {
    if pattern.is_empty() {
        return Err(Error::EmptyPattern);
    }
    let branches = split_alternation(pattern);
    let mut out: Vec<String> = Vec::with_capacity(branches.len());
    for branch in branches {
        if branch.is_empty() {
            // Preserve an explicit empty alternative (e.g. trailing `|`).
            out.push(String::new());
            continue;
        }
        if looks_like_regex(branch) {
            out.push(branch.to_string());
        } else {
            let tokens = tokenize(branch);
            if tokens.is_empty() {
                out.push(regex::escape(branch));
            } else {
                out.push(join_tokens(&tokens));
            }
        }
    }
    Ok(format!("(?i){}", out.join("|")))
}

/// Build a ready-to-compile regex from a raw user pattern.
///
/// If `expand` is true the pattern is first run through [`expand_pattern`];
/// otherwise it is compiled as-is (a plain grep). `word` wraps the result in
/// `\b...\b`. Returns the compiled byte regex (so we can match non-UTF-8 lines).
pub fn build_regex(pattern: &str, expand: bool, word: bool) -> Result<BytesRegex, Error> {
    let body = if expand {
        expand_pattern(pattern)?
    } else {
        pattern.to_string()
    };
    let full = if word {
        // Keep an explicit (?i) flag (if present) outside the \b anchors.
        if let Some(rest) = body.strip_prefix("(?i)") {
            format!(r"(?i)\b(?:{rest})\b")
        } else {
            format!(r"\b(?:{body})\b")
        }
    } else {
        body
    };
    Ok(BytesRegex::new(&full)?)
}

/// Produce the `egrep` command-line a user could run to get the same matching
/// behaviour as the native engine. Returned as argv (program first).
///
/// ```
/// use scig::expanded_egrep_command;
/// let argv = expanded_egrep_command("FooBar", &["src".into()]).unwrap();
/// assert_eq!(argv[0], "egrep");
/// assert!(argv.contains(&"-r".to_string()));
/// assert!(argv.contains(&"(?i)foo[-_ ]?bar".to_string()));
/// ```
pub fn expanded_egrep_command(pattern: &str, paths: &[String]) -> Result<Vec<String>, Error> {
    let expanded = expand_pattern(pattern)?;
    let mut argv = vec![
        "egrep".to_string(),
        "-r".to_string(),
        "-n".to_string(),
        expanded,
    ];
    if paths.is_empty() {
        argv.push(".".to_string());
    } else {
        argv.extend(paths.iter().cloned());
    }
    Ok(argv)
}

// ---------------------------------------------------------------------------
// Search engine
// ---------------------------------------------------------------------------

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// File the match came from (or a synthetic name for stdin).
    pub path: PathBuf,
    /// 1-based line number.
    pub line_no: u64,
    /// The full matching line, with trailing newline stripped.
    pub line: String,
    /// Byte offsets (within `line`) of each match span, for highlighting.
    pub spans: Vec<(usize, usize)>,
}

/// Options controlling a search run.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Honour `.gitignore`/`.ignore` and skip hidden files (ripgrep-style).
    pub respect_ignores: bool,
    /// Recurse into subdirectories.
    pub recursive: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            respect_ignores: true,
            recursive: true,
        }
    }
}

/// Search every line of a single reader, collecting matches.
pub fn search_reader<R: Read>(
    reader: R,
    re: &BytesRegex,
    display_path: &Path,
) -> Result<Vec<Match>, Error> {
    let mut out = Vec::new();
    let buf = BufReader::new(reader);
    for (idx, line) in buf.split(b'\n').enumerate() {
        let bytes = line?;
        let spans: Vec<(usize, usize)> =
            re.find_iter(&bytes).map(|m| (m.start(), m.end())).collect();
        if !spans.is_empty() {
            out.push(Match {
                path: display_path.to_path_buf(),
                line_no: idx as u64 + 1,
                line: String::from_utf8_lossy(&bytes).into_owned(),
                spans,
            });
        }
    }
    Ok(out)
}

/// Search a path: a file is read directly; a directory is walked (respecting
/// ignore rules unless disabled). Non-UTF-8 lines are matched as bytes.
pub fn search_path(
    root: &Path,
    re: &BytesRegex,
    opts: &SearchOptions,
) -> Result<Vec<Match>, Error> {
    let mut out = Vec::new();

    if root.is_file() {
        let f = File::open(root)?;
        out.extend(search_reader(f, re, root)?);
        return Ok(out);
    }

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(opts.respect_ignores)
        .git_ignore(opts.respect_ignores)
        .git_global(opts.respect_ignores)
        .git_exclude(opts.respect_ignores)
        .ignore(opts.respect_ignores)
        .parents(opts.respect_ignores);
    if !opts.recursive {
        builder.max_depth(Some(1));
    }

    for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // skip unreadable entries rather than abort
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let f = match File::open(path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        out.extend(search_reader(f, re, path)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_snake() {
        assert_eq!(tokenize("first_name"), vec!["first", "name"]);
    }

    #[test]
    fn tokenize_kebab() {
        assert_eq!(tokenize("first-name"), vec!["first", "name"]);
    }

    #[test]
    fn tokenize_camel() {
        assert_eq!(tokenize("firstName"), vec!["first", "name"]);
    }

    #[test]
    fn tokenize_pascal() {
        assert_eq!(tokenize("FirstName"), vec!["first", "name"]);
    }

    #[test]
    fn tokenize_constant() {
        assert_eq!(tokenize("FIRST_NAME"), vec!["first", "name"]);
    }

    #[test]
    fn tokenize_acronym() {
        assert_eq!(
            tokenize("getHTTPResponseCode"),
            vec!["get", "http", "response", "code"]
        );
        assert_eq!(tokenize("HTTPServer"), vec!["http", "server"]);
        assert_eq!(tokenize("parseXMLFile"), vec!["parse", "xml", "file"]);
    }

    #[test]
    fn tokenize_digits() {
        assert_eq!(tokenize("utf8"), vec!["utf", "8"]);
        assert_eq!(tokenize("parseUTF8"), vec!["parse", "utf", "8"]);
        assert_eq!(tokenize("x509Cert"), vec!["x", "509", "cert"]);
    }

    #[test]
    fn tokenize_messy_mix() {
        assert_eq!(
            tokenize("get_HTTPResponse-code"),
            vec!["get", "http", "response", "code"]
        );
    }

    #[test]
    fn tokenize_single_word() {
        assert_eq!(tokenize("foobar"), vec!["foobar"]);
        assert_eq!(tokenize("Foo"), vec!["foo"]);
    }

    #[test]
    fn expand_single() {
        assert_eq!(expand_pattern("FooBar").unwrap(), "(?i)foo[-_ ]?bar");
    }

    #[test]
    fn expand_alternation() {
        assert_eq!(
            expand_pattern("FooBar|first_name").unwrap(),
            "(?i)foo[-_ ]?bar|first[-_ ]?name"
        );
    }

    #[test]
    fn expand_passes_raw_regex_through() {
        // A branch with metacharacters is left verbatim.
        let got = expand_pattern("FooBar|ba.*z").unwrap();
        assert_eq!(got, "(?i)foo[-_ ]?bar|ba.*z");
    }

    #[test]
    fn expand_empty_is_error() {
        assert!(matches!(expand_pattern(""), Err(Error::EmptyPattern)));
    }

    #[test]
    fn expanded_regex_matches_all_conventions() {
        let re = build_regex("FooBar", true, false).unwrap();
        for form in [
            "FooBar", "fooBar", "foo_bar", "foo-bar", "FOO_BAR", "foobar",
        ] {
            assert!(
                re.is_match(form.as_bytes()),
                "expected `{form}` to match expanded FooBar"
            );
        }
        // A different identifier should NOT match.
        assert!(!re.is_match(b"barFoo"));
        assert!(!re.is_match(b"fooBaz"));
    }

    #[test]
    fn word_boundary_mode() {
        // Without word mode, a substring hit is fine.
        let loose = build_regex("FooBar", true, false).unwrap();
        assert!(loose.is_match(b"xxfoobarxx"));
        // With word mode, it must stand alone.
        let strict = build_regex("FooBar", true, true).unwrap();
        assert!(strict.is_match(b"a fooBar here"));
        assert!(!strict.is_match(b"xxfoobarxx"));
    }

    #[test]
    fn plain_mode_no_expansion() {
        // expand=false compiles the literal regex; underscores stay literal.
        let re = build_regex("foo_bar", false, false).unwrap();
        assert!(re.is_match(b"foo_bar"));
        assert!(!re.is_match(b"fooBar"));
    }

    #[test]
    fn search_reader_finds_lines() {
        let re = build_regex("FooBar", true, false).unwrap();
        let text = "let foo_bar = 1;\nlet other = 2;\nfn fooBar() {}\n";
        let matches = search_reader(text.as_bytes(), &re, Path::new("<test>")).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_no, 1);
        assert_eq!(matches[1].line_no, 3);
        assert_eq!(matches[0].spans, vec![(4, 11)]);
    }

    #[test]
    fn search_reader_handles_non_utf8() {
        let re = build_regex("FooBar", true, false).unwrap();
        // valid line, then a line with an invalid UTF-8 byte 0xff
        let mut data = b"foo_bar ok\n".to_vec();
        data.extend_from_slice(&[0xff, b'f', b'o', b'o', b'B', b'a', b'r', b'\n']);
        let matches = search_reader(&data[..], &re, Path::new("<test>")).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[1].line_no, 2);
    }

    #[test]
    fn egrep_command_shape() {
        let argv = expanded_egrep_command("first_name", &[]).unwrap();
        assert_eq!(argv[0], "egrep");
        assert_eq!(argv.last().unwrap(), ".");
        assert!(argv.iter().any(|a| a == "(?i)first[-_ ]?name"));
    }
}
