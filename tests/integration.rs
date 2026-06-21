//! End-to-end test of the native search engine against an on-disk fixture
//! tree. This exercises directory walking + matching together, not just the
//! pure pattern logic in the unit tests.

use std::fs;
use std::path::Path;

use scig::{build_regex, search_path, SearchOptions};

/// Build a small fixture tree in a temp dir containing the same identifier in
/// several conventions, plus a decoy, plus an ignored file.
fn make_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("nested/deep")).unwrap();

    fs::write(
        root.join("src/camel.js"),
        "function fooBar() { return 1; }\nconst other = 2;\n",
    )
    .unwrap();
    fs::write(
        root.join("src/snake.py"),
        "def foo_bar():\n    return baz\n",
    )
    .unwrap();
    fs::write(root.join("src/constant.h"), "#define FOO_BAR 42\n").unwrap();
    fs::write(
        root.join("nested/deep/kebab.css"),
        ".foo-bar { color: red; }\n",
    )
    .unwrap();
    // A decoy that must NOT match `FooBar`.
    fs::write(root.join("nested/decoy.txt"), "barFoo and fooBaz\n").unwrap();
}

#[test]
fn finds_identifier_in_every_convention_across_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_fixture(root);

    let re = build_regex("FooBar", true, false).unwrap();
    let mut matches = search_path(root, &re, &SearchOptions::default()).unwrap();
    matches.sort_by(|a, b| a.path.cmp(&b.path).then(a.line_no.cmp(&b.line_no)));

    // camel + snake + constant + kebab = 4 matching lines, decoy excluded.
    assert_eq!(
        matches.len(),
        4,
        "expected 4 matches, got: {:#?}",
        matches
            .iter()
            .map(|m| (m.path.clone(), m.line.clone()))
            .collect::<Vec<_>>()
    );
    assert!(matches.iter().all(|m| !m.line.contains("barFoo")));
    assert!(matches.iter().all(|m| !m.line.contains("fooBaz")));
}

#[test]
fn fixed_mode_only_matches_literal() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_fixture(root);

    // Plain (non-expanded) search for the literal snake form.
    let re = build_regex("foo_bar", false, false).unwrap();
    let matches = search_path(root, &re, &SearchOptions::default()).unwrap();
    assert_eq!(matches.len(), 1);
    assert!(matches[0].line.contains("foo_bar"));
}

#[test]
fn respects_dot_ignore_file_when_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_fixture(root);
    // A `.ignore` file is honoured by the `ignore` crate whether or not the
    // tree is a git repository, so it is the portable way to test the
    // ignore-respecting code path. (`.gitignore` is only authoritative inside
    // an actual git repo, which we cover separately below.)
    fs::write(root.join(".ignore"), "nested/\n").unwrap();

    let re = build_regex("FooBar", true, false).unwrap();

    let respecting = search_path(root, &re, &SearchOptions::default()).unwrap();
    // nested/deep/kebab.css is now ignored -> 3 matches (camel, snake, constant).
    assert_eq!(
        respecting.len(),
        3,
        "ignored matches: {:#?}",
        respecting
            .iter()
            .map(|m| m.path.clone())
            .collect::<Vec<_>>()
    );

    let ignoring = search_path(
        root,
        &re,
        &SearchOptions {
            respect_ignores: false,
            recursive: true,
        },
    )
    .unwrap();
    // The `.ignore` file is itself a non-matching dotfile; with ignores off we
    // also descend hidden files, but none of them match -> still 4 matches.
    assert_eq!(ignoring.len(), 4);
}

#[test]
fn respects_gitignore_inside_a_git_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_fixture(root);
    // The `ignore` crate only treats `.gitignore` as authoritative when it can
    // see a git repo. A bare `.git/` directory is enough to flip that on.
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".gitignore"), "nested/\n").unwrap();

    let re = build_regex("FooBar", true, false).unwrap();
    let respecting = search_path(root, &re, &SearchOptions::default()).unwrap();
    assert_eq!(
        respecting.len(),
        3,
        "ignored matches: {:#?}",
        respecting
            .iter()
            .map(|m| m.path.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn non_recursive_stays_top_level() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    make_fixture(root);

    let re = build_regex("FooBar", true, false).unwrap();
    let matches = search_path(
        root,
        &re,
        &SearchOptions {
            respect_ignores: true,
            recursive: false,
        },
    )
    .unwrap();
    // Only top-level files; everything here lives under src/ or nested/, so 0.
    assert_eq!(matches.len(), 0);
}
