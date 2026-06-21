//! `scig` — super-case-insensitive-grep.
//!
//! A small grep clone whose headline feature is `--ident` (the default) mode:
//! a single identifier pattern matches the same name written in *any* common
//! naming convention. `scig 'FooBar|first_name' src/` finds `fooBar`,
//! `foo_bar`, `FOO_BAR`, `first-name`, ... in one pass.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use scig::{
    build_regex, expand_pattern, expanded_egrep_command, search_path, search_reader, Match,
    SearchOptions,
};

const HELP: &str = "\
scig — super-case-insensitive-grep

grep, but case-STYLE-insensitive: one identifier pattern matches camelCase,
snake_case, PascalCase, kebab-case and CONSTANT_CASE at once.

USAGE:
    scig [OPTIONS] <PATTERN> [PATH ...]

    PATTERN   identifier(s) to find, e.g. 'FooBar' or 'FooBar|first_name'.
              Branches that contain regex metacharacters are used verbatim.
    PATH      files/dirs to search (default: current directory; '-' = stdin).

OPTIONS:
    -i, --ident        expand the pattern across naming conventions (DEFAULT).
    -F, --fixed        do NOT expand; treat PATTERN as an ordinary regex (plain grep).
    -w, --word         match whole words only (wrap in word boundaries).
    -a, --all          do NOT respect .gitignore/.ignore or skip hidden files.
        --no-recursive search only the top level of each directory.
        --show-regex   print the expanded regex and exit (no search).
        --print-egrep  print an equivalent `egrep` command and exit (no search).
        --no-color     never colorize matches.
        --color        always colorize matches (default: auto when stdout is a tty).
    -c, --count        print only a count of matching lines.
    -h, --help         show this help.
    -V, --version      show version.

NOTE:
    Convention-expansion turns one identifier into an alternation with optional
    separators, so on very large trees it can be noticeably slower than a plain
    fixed-string grep. Narrow the PATH or use --fixed for hot paths.
";

#[derive(Default)]
struct Args {
    pattern: Option<String>,
    paths: Vec<String>,
    expand: bool,
    word: bool,
    all: bool,
    recursive: bool,
    show_regex: bool,
    print_egrep: bool,
    count: bool,
    color: ColorChoice,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

impl Default for ColorChoice {
    fn default() -> Self {
        ColorChoice::Auto
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("scig: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&raw)? {
        Some(a) => a,
        None => return Ok(ExitCode::SUCCESS), // help/version already printed
    };

    let pattern = match args.pattern {
        Some(ref p) => p.clone(),
        None => {
            eprintln!("scig: missing PATTERN\n\n{HELP}");
            return Ok(ExitCode::from(2));
        }
    };

    // Informational modes that do no searching.
    if args.show_regex {
        println!("{}", expand_pattern(&pattern)?);
        return Ok(ExitCode::SUCCESS);
    }
    if args.print_egrep {
        let argv = expanded_egrep_command(&pattern, &args.paths)?;
        println!("{}", shell_join(&argv));
        return Ok(ExitCode::SUCCESS);
    }

    let re = build_regex(&pattern, args.expand, args.word)?;

    let opts = SearchOptions {
        respect_ignores: !args.all,
        recursive: args.recursive,
    };

    // Gather matches across every requested path (or stdin).
    let mut all_matches: Vec<Match> = Vec::new();
    if args.paths.is_empty() {
        all_matches.extend(search_path(Path::new("."), &re, &opts)?);
    } else {
        for p in &args.paths {
            if p == "-" {
                let stdin = io::stdin();
                all_matches.extend(search_reader(stdin.lock(), &re, Path::new("<stdin>"))?);
            } else {
                all_matches.extend(search_path(Path::new(p), &re, &opts)?);
            }
        }
    }

    let use_color = match args.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => io::stdout().is_terminal(),
    };

    let stdout = io::stdout();
    let mut out = stdout.lock();

    if args.count {
        writeln!(out, "{}", all_matches.len())?;
    } else {
        let show_paths = should_show_paths(&args.paths);
        for m in &all_matches {
            print_match(&mut out, m, use_color, show_paths)?;
        }
    }

    Ok(if all_matches.is_empty() {
        ExitCode::from(1) // grep convention: no matches -> exit 1
    } else {
        ExitCode::SUCCESS
    })
}

/// Decide whether to prefix each match with its filename, matching grep/ripgrep
/// behaviour: filenames are shown whenever results may span multiple files —
/// i.e. always, *except* when the user named exactly one explicit regular file
/// (or stdin). A single directory argument still recurses across many files, so
/// it shows filenames.
fn should_show_paths(paths: &[String]) -> bool {
    if paths.is_empty() {
        // Default search of "." recurses across files -> show filenames.
        return true;
    }
    if paths.len() > 1 {
        return true;
    }
    let only = &paths[0];
    if only == "-" {
        return false; // stdin: nothing useful to name
    }
    // One path: show filenames unless it is a single regular file. If we cannot
    // stat it (or it is a directory), default to showing the path.
    !Path::new(only).is_file()
}

fn print_match<W: Write>(out: &mut W, m: &Match, color: bool, show_path: bool) -> io::Result<()> {
    let line = highlight(&m.line, &m.spans, color);
    if show_path {
        if color {
            writeln!(
                out,
                "\x1b[35m{}\x1b[0m:\x1b[32m{}\x1b[0m:{}",
                m.path.display(),
                m.line_no,
                line
            )
        } else {
            writeln!(out, "{}:{}:{}", m.path.display(), m.line_no, line)
        }
    } else if color {
        writeln!(out, "\x1b[32m{}\x1b[0m:{}", m.line_no, line)
    } else {
        writeln!(out, "{}:{}", m.line_no, line)
    }
}

/// Wrap each matched span in bold-red ANSI codes (or return the line as-is).
fn highlight(line: &str, spans: &[(usize, usize)], color: bool) -> String {
    if !color || spans.is_empty() {
        return line.to_string();
    }
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len() + spans.len() * 11);
    let mut cursor = 0usize;
    for &(s, e) in spans {
        // Spans are byte offsets from the regex engine; clamp defensively.
        let (s, e) = (s.min(bytes.len()), e.min(bytes.len()));
        if s < cursor {
            continue;
        }
        out.push_str(&line[cursor..s]);
        out.push_str("\x1b[1;31m");
        out.push_str(&line[s..e]);
        out.push_str("\x1b[0m");
        cursor = e;
    }
    out.push_str(&line[cursor..]);
    out
}

fn parse_args(raw: &[String]) -> Result<Option<Args>, String> {
    let mut a = Args {
        expand: true,
        recursive: true,
        ..Default::default()
    };
    let mut positional_only = false;
    let mut i = 0;
    while i < raw.len() {
        let arg = &raw[i];
        if positional_only || !arg.starts_with('-') || arg == "-" {
            push_positional(&mut a, arg.clone());
            i += 1;
            continue;
        }
        match arg.as_str() {
            "--" => positional_only = true,
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("scig {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "-i" | "--ident" => a.expand = true,
            "-F" | "--fixed" | "--no-ident" => a.expand = false,
            "-w" | "--word" => a.word = true,
            "-a" | "--all" => a.all = true,
            "--no-recursive" => a.recursive = false,
            "--show-regex" => a.show_regex = true,
            "--print-egrep" => a.print_egrep = true,
            "-c" | "--count" => a.count = true,
            "--no-color" => a.color = ColorChoice::Never,
            "--color" => a.color = ColorChoice::Always,
            other => {
                // Support bundled short flags like -iw.
                if other.starts_with('-') && !other.starts_with("--") && other.len() > 2 {
                    for ch in other[1..].chars() {
                        match ch {
                            'i' => a.expand = true,
                            'F' => a.expand = false,
                            'w' => a.word = true,
                            'a' => a.all = true,
                            'c' => a.count = true,
                            _ => return Err(format!("unknown flag -{ch} (in {other})")),
                        }
                    }
                } else {
                    return Err(format!("unknown option: {other}"));
                }
            }
        }
        i += 1;
    }
    Ok(Some(a))
}

fn push_positional(a: &mut Args, value: String) {
    if a.pattern.is_none() {
        a.pattern = Some(value);
    } else {
        a.paths.push(value);
    }
}

/// Render an argv as a copy-pasteable shell command, quoting args that need it.
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty()
                || a.chars()
                    .any(|c| c.is_whitespace() || "()|*?[]{}$\\\"'`&;<>".contains(c))
            {
                format!("'{}'", a.replace('\'', r"'\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults_to_expand() {
        let a = parse_args(&["FooBar".to_string()]).unwrap().unwrap();
        assert!(a.expand);
        assert_eq!(a.pattern.as_deref(), Some("FooBar"));
        assert!(a.paths.is_empty());
    }

    #[test]
    fn parse_fixed_flag() {
        let a = parse_args(&["-F".to_string(), "foo".to_string()])
            .unwrap()
            .unwrap();
        assert!(!a.expand);
    }

    #[test]
    fn parse_bundled_short_flags() {
        let a = parse_args(&["-wc".to_string(), "foo".to_string()])
            .unwrap()
            .unwrap();
        assert!(a.word);
        assert!(a.count);
        assert!(a.expand); // unchanged default
    }

    #[test]
    fn parse_paths_after_pattern() {
        let a = parse_args(&["Foo".to_string(), "src".to_string(), "tests".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(a.pattern.as_deref(), Some("Foo"));
        assert_eq!(a.paths, vec!["src".to_string(), "tests".to_string()]);
    }

    #[test]
    fn parse_double_dash_pattern_starting_with_dash() {
        let a = parse_args(&["--".to_string(), "-weird".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(a.pattern.as_deref(), Some("-weird"));
    }

    #[test]
    fn parse_unknown_option_errors() {
        assert!(parse_args(&["--nope".to_string()]).is_err());
    }

    #[test]
    fn highlight_wraps_span() {
        let h = highlight("a fooBar b", &[(2, 8)], true);
        assert!(h.contains("\x1b[1;31mfooBar\x1b[0m"));
    }

    #[test]
    fn highlight_noop_without_color() {
        let h = highlight("a fooBar b", &[(2, 8)], false);
        assert_eq!(h, "a fooBar b");
    }

    #[test]
    fn shell_join_quotes_metachars() {
        let s = shell_join(&["egrep".into(), "(?i)foo".into()]);
        assert_eq!(s, "egrep '(?i)foo'");
    }

    #[test]
    fn show_paths_default_and_multi() {
        // Default (".") and multiple paths always show filenames.
        assert!(should_show_paths(&[]));
        assert!(should_show_paths(&["a".into(), "b".into()]));
    }

    #[test]
    fn show_paths_stdin_is_false() {
        assert!(!should_show_paths(&["-".into()]));
    }

    #[test]
    fn show_paths_single_dir_true_single_file_false() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_string_lossy().into_owned();
        // A single directory argument recurses across files -> show filenames.
        assert!(should_show_paths(&[dir]));

        let file = tmp.path().join("only.txt");
        std::fs::write(&file, b"hi\n").unwrap();
        // A single regular file -> suppress the (redundant) filename.
        assert!(!should_show_paths(&[file.to_string_lossy().into_owned()]));
    }
}
