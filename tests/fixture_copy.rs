//! The one copy of a fixture repo that every crate's tests make (#314),
//! included by path from their `tests/common`, so the crates skip the same
//! build output.

use std::path::Path;

/// What a build, an IDE or a package manager leaves in a fixture's tree,
/// listed in the fixtures' `.gitignore`: never part of the repo a test
/// works on. An IDE that restores the C# fixtures in place writes `bin/`
/// and `obj/` there, and the gate would read `obj/*.nuget.g.props` as a
/// changed lockfile.
pub const BUILD_OUTPUT: &[&str] = &[
    "target",
    "__pycache__",
    ".pytest_cache",
    ".venv",
    "node_modules",
    "bin",
    "obj",
    "TestResults",
];

/// `from` to `to`, without the directories of [`BUILD_OUTPUT`] at any depth.
pub fn copy_fixture(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let target = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            if !BUILD_OUTPUT.iter().any(|s| name == *s) {
                copy_fixture(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
