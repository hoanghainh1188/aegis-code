//! False-positive check of the injection flags (#444): every tracked text
//! file of the given git repos at HEAD is scanned, and each hit printed.
//!
//! ```text
//! cargo run -p aegis-lifecycle --example flag_scan -- .workspace/repos/gin .workspace/repos/gson
//! ```

use std::path::Path;
use std::process::Command;

fn files(repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-files", "-z"])
        .output()
        .expect("git runs");
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect()
}

fn main() {
    let (mut total_files, mut total_lines, mut total_hits) = (0, 0, 0);
    for repo in std::env::args().skip(1) {
        let repo = Path::new(&repo);
        let (mut lines, mut hits) = (0, 0);
        let mut scanned = 0;
        for file in files(repo) {
            let Ok(bytes) = std::fs::read(repo.join(&file)) else {
                continue;
            };
            if bytes.len() > 1 << 20 {
                continue;
            }
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            scanned += 1;
            lines += text.lines().count();
            for hit in aegis_lifecycle::flags::detect(&text) {
                hits += 1;
                println!(
                    "{}:{}:{}\t[{}]\t{}",
                    repo.display(),
                    file,
                    hit.line,
                    hit.rule,
                    hit.excerpt
                );
            }
        }
        println!(
            "== {}: {scanned} files, {lines} lines, {hits} hits",
            repo.display()
        );
        total_files += scanned;
        total_lines += lines;
        total_hits += hits;
    }
    println!("== total: {total_files} files, {total_lines} lines, {total_hits} hits");
}
