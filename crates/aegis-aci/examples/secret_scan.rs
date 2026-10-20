//! Counts what the secret scanner would hide in a repo's text files, to
//! check its false positives on real code.
//!
//! `cargo run -p aegis-aci --example secret_scan -- <repo>`

use std::collections::BTreeMap;
use std::path::Path;

fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if [".git", "target", "node_modules", ".venv"]
            .iter()
            .any(|s| name == *s)
        {
            continue;
        }
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&path, files),
            Ok(t) if t.is_file() => files.push(path),
            _ => {}
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).expect("usage: secret_scan <repo>");
    let mut files = Vec::new();
    walk(Path::new(&root), &mut files);
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut scanned = 0;
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        scanned += 1;
        let r = aegis_aci::redact(&text);
        for (kind, n) in &r.hidden {
            *kinds.entry(kind).or_default() += n;
            println!("{}\t{kind}\t{n}", file.display());
        }
    }
    eprintln!("{scanned} text files, hidden: {kinds:?}");
}
