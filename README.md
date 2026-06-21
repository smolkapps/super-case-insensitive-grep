# super-case-insensitive-grep (`scig`)

`grep`, but **case-STYLE-insensitive**. You give it one identifier and it finds
that identifier written in *every* common naming convention at once — no more
running four searches for `fooBar`, `foo_bar`, `FOO_BAR`, and `foo-bar`.

```sh
scig 'FooBar' src/
```

matches all of:

| Convention      | Example   |
| --------------- | --------- |
| `camelCase`     | `fooBar`  |
| `PascalCase`    | `FooBar`  |
| `snake_case`    | `foo_bar` |
| `kebab-case`    | `foo-bar` |
| `CONSTANT_CASE` | `FOO_BAR` |
| squashed        | `foobar`  |

…but **not** unrelated names like `barFoo` or `fooBaz`.

## How it works

The pattern is tokenized into words and rebuilt into a single case-insensitive
regex with an optional separator between tokens:

```
FooBar              ->  (?i)foo[-_ ]?bar
FooBar|first_name   ->  (?i)foo[-_ ]?bar|first[-_ ]?name
```

The tokenizer understands camelCase/Pascal boundaries, acronyms
(`getHTTPResponseCode` -> `get http response code`), and letter/digit
transitions (`parseUTF8` -> `parse utf 8`). It splits on `_`, `-`, space and
`.`, so a messy mix like `get_HTTPResponse-code` still expands correctly.

Run `--show-regex` to see exactly what a pattern expands to:

```sh
$ scig --show-regex 'FooBar|first_name'
(?i)foo[-_ ]?bar|first[-_ ]?name
```

## Performance note

Convention-expansion turns one identifier into an alternation with optional
separators, which is inherently more work than a fixed-string search. On very
large trees it can be **noticeably slower** than a plain `grep -F`. Narrow the
search path, or pass `--fixed` to disable expansion on hot paths.

The default engine is native (built on the `regex` and `ignore` crates, the same
directory-walking library ripgrep uses), so it honours `.gitignore`/`.ignore`
and skips hidden files unless you pass `--all`. If you would rather run the
expansion through your system `egrep`, `--print-egrep` prints an equivalent
command you can pipe to a shell.

## Install / build from source

Requires a Rust toolchain (`rustup`, stable). No other system dependencies.

```sh
git clone <this-repo>
cd super-case-insensitive-grep
cargo build --release
# binary at: target/release/scig

# or install onto your PATH:
cargo install --path .
```

## Usage

```text
scig [OPTIONS] <PATTERN> [PATH ...]

  PATTERN   identifier(s) to find, e.g. 'FooBar' or 'FooBar|first_name'.
            Branches containing regex metacharacters are used verbatim.
  PATH      files/dirs to search (default: current directory; '-' = stdin).

OPTIONS
  -i, --ident         expand across naming conventions (DEFAULT)
  -F, --fixed         do NOT expand; PATTERN is an ordinary regex (plain grep)
  -w, --word          match whole words only
  -a, --all           do NOT respect .gitignore/.ignore or skip hidden files
      --no-recursive  search only the top level of each directory
      --show-regex    print the expanded regex and exit
      --print-egrep   print an equivalent egrep command and exit
      --color/--no-color   force color on/off (default: auto)
  -c, --count         print only a count of matching lines
  -h, --help          show help
  -V, --version       show version
```

### Examples

```sh
# Find a config key however it's spelled, anywhere in the repo:
scig maxRetries

# Two identifiers at once, whole-word only:
scig -w 'userId|account_number' src/

# Mix expansion with raw regex (the second branch is verbatim):
scig 'FooBar|TODO.*FIXME'

# Pipe input:
cat app.log | scig requestId -

# See the regex without searching:
scig --show-regex getHTTPResponseCode
# -> (?i)get[-_ ]?http[-_ ]?response[-_ ]?code

# Print an egrep command instead of searching natively:
scig --print-egrep first_name src/
# -> egrep -r -n '(?i)first[-_ ]?name' src/
```

## Exit codes

Follows the `grep` convention:

* `0` — at least one match
* `1` — no matches
* `2` — error (bad pattern, bad flags, …)

## Tests

```sh
cargo test
```

Unit tests cover the tokenizer, pattern expansion, word-boundary mode, plain
mode, non-UTF-8 lines, and argument parsing. An integration test runs the full
search engine against an on-disk fixture tree (including `.gitignore`
handling). No test touches the network or any external service.

## License

MIT — see [LICENSE](LICENSE).
