//! The patch a job's PR holds (#312), split file by file for the job's
//! page and the PR's review page. Plain text in, nothing interpreted: the
//! template escapes every line.

use aegis_store::{JobId, Patch, Store};

use super::WebError;

pub(super) struct PatchView {
    pub files: Vec<FileDiff>,
    /// The kept patch was cut; the PR has it whole.
    pub truncated: bool,
}

pub(super) struct FileDiff {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<DiffLine>,
}

pub(super) struct DiffLine {
    /// `add`, `del`, `hunk`, `meta` or `ctx`.
    pub class: &'static str,
    pub text: String,
}

/// `git diff` output, one entry per `diff --git a/… b/…` header.
pub(super) fn view(patch: &Patch) -> PatchView {
    let mut files: Vec<FileDiff> = Vec::new();
    for line in patch.diff.lines() {
        if let Some(header) = line.strip_prefix("diff --git ") {
            let path = header
                .rsplit_once(" b/")
                .map_or(header, |(_, b)| b)
                .to_owned();
            files.push(FileDiff {
                path,
                added: 0,
                removed: 0,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        let class = if line.starts_with("@@") {
            "hunk"
        } else if line.starts_with("+++ ") || line.starts_with("--- ") {
            "meta"
        } else if line.starts_with('+') {
            file.added += 1;
            "add"
        } else if line.starts_with('-') {
            file.removed += 1;
            "del"
        } else if line.starts_with(' ') || line.is_empty() {
            "ctx"
        } else {
            // `index …`, `new file mode`, `rename from`, `\ No newline…`.
            "meta"
        };
        file.lines.push(DiffLine {
            class,
            text: line.to_owned(),
        });
    }
    PatchView {
        files,
        truncated: patch.truncated,
    }
}

/// `job`'s patch as a view, when its PR was opened with one.
pub(super) async fn of(store: &Store, job: JobId) -> Result<Option<PatchView>, WebError> {
    Ok(store.patch(job).await?.map(|p| view(&p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_diff_by_file_and_counts_lines() {
        let patch = Patch {
            diff: "\
diff --git a/src/lib.rs b/src/lib.rs
index 1..2 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 keep
-old
+new
diff --git a/tests/t.rs b/tests/t.rs
new file mode 100644
--- /dev/null
+++ b/tests/t.rs
@@ -0,0 +1,2 @@
+a
+b
"
            .into(),
            truncated: true,
        };
        let v = view(&patch);
        assert!(v.truncated);
        let summary: Vec<_> = v
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.added, f.removed))
            .collect();
        assert_eq!(summary, [("src/lib.rs", 1, 1), ("tests/t.rs", 2, 0)]);
        let classes: Vec<_> = v.files[0].lines.iter().map(|l| l.class).collect();
        assert_eq!(
            classes,
            ["meta", "meta", "meta", "hunk", "ctx", "del", "add"]
        );
    }
}
