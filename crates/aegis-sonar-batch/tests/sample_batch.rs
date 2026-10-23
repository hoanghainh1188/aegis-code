//! The sample batch in `tests/fixtures/sample`: five Sonar warnings on a
//! small Python package, grouped, worked on by stand-in sessions (commits
//! made here) and merged with a stand-in gate. One group conflicts with
//! another, one fails the gate once merged, one session stops.

use std::future::{Future, ready};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use aegis_lang::Language;
use aegis_sonar_batch::{
    Author, BatchError, Checked, Dropped, Group, IntegrationTree, Steps, group, integrate,
    integration_branch, parse_search,
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample");
const STORE: &str = "pkg/store.py";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A stand-in session: `edits` on `STORE` at `base`, committed.
fn session(repo: &Path, base: &str, edits: &[(&str, &str)], name: &str) -> String {
    git(repo, &["checkout", "-q", "--detach", base]);
    let path = repo.join(STORE);
    let text = edits
        .iter()
        .fold(std::fs::read_to_string(&path).unwrap(), |t, (from, to)| {
            assert!(t.contains(from), "{from}");
            t.replacen(from, to, 1)
        });
    std::fs::write(&path, text).unwrap();
    git(repo, &["commit", "-qam", name]);
    git(repo, &["rev-parse", "HEAD"])
}

const LOAD_FIX: &[(&str, &str)] = &[(
    "    data = open(path).read()\n    unused = 1\n",
    "    with open(path) as f:\n        data = f.read()\n",
)];
const GET_FIX: &[(&str, &str)] = &[("        value = None\n", "")];
const GET_BROKEN: &[(&str, &str)] = &[("        value = None\n", "        BROKEN\n")];
/// The flow's first fix also checks the path in `load`, where group 1
/// changed the same lines.
const SAVE_FIX_TOUCHING_LOAD: &[(&str, &str)] = &[
    (
        "    data = open(path).read()\n",
        "    data = open(os.path.basename(path)).read()\n",
    ),
    (
        "    tmp = path + \".tmp\"\n",
        "    tmp = os.path.basename(path) + \".tmp\"\n",
    ),
];
const SAVE_FIX: &[(&str, &str)] = &[(
    "    tmp = path + \".tmp\"\n",
    "    tmp = os.path.basename(path) + \".tmp\"\n",
)];

struct StandIn {
    repo: PathBuf,
    gates: Mutex<u32>,
    reruns: Mutex<Vec<String>>,
}

impl Steps for StandIn {
    /// The gate run's number.
    type Report = u32;
    type Error = BatchError;

    fn gate(&self, tree: &Path) -> impl Future<Output = Result<Checked<u32>, BatchError>> + Send {
        let mut n = self.gates.lock().unwrap();
        *n += 1;
        let text = std::fs::read_to_string(tree.join(STORE)).unwrap();
        ready(Ok(Checked {
            passed: !text.contains("BROKEN"),
            report: *n,
        }))
    }

    fn rerun(
        &self,
        group: &Group,
        base: &str,
    ) -> impl Future<Output = Result<Result<String, String>, BatchError>> + Send {
        self.reruns.lock().unwrap().push(group.id.clone());
        let edits = match group.id.as_str() {
            "g2" => GET_FIX,
            "g4" => SAVE_FIX,
            other => panic!("no rerun expected for {other}"),
        };
        ready(Ok(Ok(session(&self.repo, base, edits, &group.id))))
    }
}

#[tokio::test]
async fn the_sample_batch_ends_in_one_integration_branch() {
    let root = std::env::temp_dir().join(format!("aegis-sonar-batch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    copy_dir(Path::new(FIXTURE), &repo);
    std::fs::remove_file(repo.join("issues.json")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    let json = std::fs::read_to_string(Path::new(FIXTURE).join("issues.json")).unwrap();
    let groups = group(&parse_search(&json).unwrap(), &repo, Language::Python);
    let scopes: Vec<(String, Vec<&str>)> = groups
        .iter()
        .map(|g| (g.scope.describe(), g.keys()))
        .collect();
    assert_eq!(
        scopes,
        vec![
            (
                "pkg/store.py:4-7 (`load`)".to_owned(),
                vec!["AX-load-open", "AX-load-unused"]
            ),
            (
                "pkg/store.py:18-20 (`Store.get`)".to_owned(),
                vec!["AX-get-unused"]
            ),
            ("pkg/util.py:1-2 (`shout`)".to_owned(), vec!["AX-shout-doc"]),
            (
                "data flow of issue AX-save-path".to_owned(),
                vec!["AX-save-path"]
            ),
        ]
    );

    // The sessions, side by side in the real run, each from the base.
    let branches: Vec<(Group, Result<String, String>)> = groups
        .into_iter()
        .map(|g| {
            let result = match g.id.as_str() {
                "g1" => Ok(session(&repo, &base, LOAD_FIX, "g1")),
                "g2" => Ok(session(&repo, &base, GET_BROKEN, "g2")),
                "g3" => Err("budget".to_owned()),
                _ => Ok(session(&repo, &base, SAVE_FIX_TOUCHING_LOAD, "g4")),
            };
            (g, result)
        })
        .collect();

    let checkout = root.join("integration");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            checkout.to_str().unwrap(),
            &base,
        ],
    );
    let branch = integration_branch("sample").unwrap();
    let author = Author {
        name: "aegis".into(),
        email: "aegis@example.com".into(),
    };
    let tree = IntegrationTree::start(&checkout, &branch, author).unwrap();
    let steps = StandIn {
        repo: repo.clone(),
        gates: Mutex::new(0),
        reruns: Mutex::new(Vec::new()),
    };
    let done = integrate(&steps, &tree, branches).await.unwrap();

    assert_eq!(done.merged, ["g1", "g2", "g4"]);
    assert_eq!(done.retried, ["g2", "g4"]);
    assert_eq!(
        done.dropped,
        vec![("g3".to_owned(), Dropped::Stopped("budget".into()))]
    );
    assert_eq!(*steps.reruns.lock().unwrap(), ["g2", "g4"]);
    // g1, g2 (failed), g2 again, g4 again: the conflict ran no gate.
    assert_eq!(done.report, Some(4));
    assert_eq!(
        git(&repo, &["rev-parse", &format!("refs/heads/{branch}")]),
        done.head
    );
    let merged = git(&repo, &["show", &format!("{branch}:{STORE}")]);
    assert!(merged.contains("with open(path) as f:"));
    assert!(!merged.contains("value = None"));
    assert!(!merged.contains("BROKEN"));
    assert!(merged.contains("tmp = os.path.basename(path)"));
    assert!(!merged.contains("open(os.path.basename(path))"));
    // Every merge on the branch passed the gate: no conflict markers, and
    // the work tree is clean.
    assert_eq!(git(&checkout, &["status", "--porcelain"]), "");

    drop(tree);
    let _ = std::fs::remove_dir_all(&root);
}
