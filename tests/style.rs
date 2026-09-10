//! The prose rules from CONTRIBUTING.md, checked rather than remembered.
//!
//! The rudb repository has this as a `cargo xtask` task because it has an xtask crate for the
//! layer rule anyway. Here there is no such crate and no reason to add one, so it is a test.

use std::path::{Path, PathBuf};

#[test]
fn the_prose_rules_hold() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "no markdown found, which means this test is checking nothing");

    let mut problems = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("a file the walk just found");
        let name = file.strip_prefix(root).unwrap_or(file).display().to_string();
        problems.extend(check(&name, &text));
    }

    assert!(problems.is_empty(), "prose violations:\n  {}", problems.join("\n  "));
}

fn check(name: &str, text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut problems = Vec::new();
    let mut in_code = false;

    for (i, line) in lines.iter().enumerate() {
        let number = i + 1;
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if let Some(column) = line.find(['\u{2014}', '\u{2013}']) {
            problems.push(format!("{name}:{number}:{column}: an em or en dash"));
        }
        if matches!(line.trim(), "---" | "***" | "___" | "- - -" | "* * *") {
            problems.push(format!("{name}:{number}: a horizontal rule"));
        }
        if is_prose(line) && continues(lines.get(i + 1).copied()) {
            problems.push(format!("{name}:{number}: a sentence broken across two lines"));
        }
    }
    problems
}

/// Ordinary paragraph text, as opposed to a heading, a list item, a table row or an indented
/// block. Only paragraphs get the one-line-per-paragraph rule, because the rest is structure and
/// wraps for reasons of its own.
fn is_prose(line: &str) -> bool {
    if line.is_empty() || line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    let first = line.chars().next().unwrap_or(' ');
    !matches!(first, '#' | '|' | '>' | '-' | '*' | '+' | '[' | '!' | '<')
        && !line.starts_with("1.")
        && !line.ends_with("  ")
}

fn continues(next: Option<&str>) -> bool {
    let Some(next) = next else { return false };
    if !is_prose(next) {
        return false;
    }
    let first = next.chars().next().unwrap_or(' ');
    first.is_lowercase() || first == ',' || first == ')'
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
}
