//! The platform's clone of a registered repo (#234): made when the repo is
//! added, its main branch fetched again before each job, so a job never
//! starts from old code. Jobs only read revisions from it and take their
//! own worktree ([`crate::job`]), so a new clone is bare.
//!
//! Git runs without global or system config, and the URL and branch are
//! the registry's, passed on each call: the clone's `origin` is not used.
//! The token goes in an HTTP header through git's environment
//! ([`aegis_pr::token_env`]), only over HTTPS. Before a call with the token
//! the clone's own config is checked for keys that could send it elsewhere
//! or run a program (`url.*.insteadOf`, `http.*`, `credential.*`, includes,
//! `core.sshCommand` …): the clone is shared by every job of its repo.

use std::path::Path;
use std::process::Command;

use crate::LifecycleError;

/// A stalled transfer is dropped after this long under 1 KB/s.
const LOW_SPEED_TIME_S: &str = "60";

/// Keys of the clone's config that could redirect a fetch, add headers,
/// read credentials or run a program.
const UNSAFE_KEYS: &str = r"^(url\..*|http\..*|credential.*|include\..*|includeif\..*|core\.(sshcommand|gitproxy|askpass|fsmonitor|hookspath))$";

/// Where a repo's code comes from: its URL in the registry and, for a
/// private one, the fork's token.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub url: &'a str,
    pub token: Option<&'a str>,
}

impl std::fmt::Debug for Source<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("url", &self.url)
            .field("token", &self.token.map(|_| "…"))
            .finish()
    }
}

impl Source<'_> {
    /// The token, only where it goes over HTTPS.
    fn token(&self) -> Option<&str> {
        self.token
            .filter(|_| self.url.to_ascii_lowercase().starts_with("https://"))
    }
}

fn git(dir: Option<&Path>, args: &[&str], token: Option<&str>) -> Result<String, LifecycleError> {
    let mut c = Command::new("git");
    c.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "http.lowSpeedLimit=1000"])
        .args(["-c", &format!("http.lowSpeedTime={LOW_SPEED_TIME_S}")]);
    if let Some(dir) = dir {
        c.arg("-C").arg(dir);
    }
    if let Some(token) = token {
        c.envs(aegis_pr::token_env(token));
    }
    let shown = args.join(" ");
    let out = c.args(args).output().map_err(|e| LifecycleError::Git {
        args: shown.clone(),
        message: e.to_string(),
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(LifecycleError::Git {
            args: shown,
            message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

/// A branch name git takes, so it cannot be read as an option or a
/// refspec.
fn check_branch(branch: &str) -> Result<(), LifecycleError> {
    let ok = !branch.starts_with('-')
        && git(None, &["check-ref-format", "--branch", branch], None).is_ok_and(|b| b == branch);
    if ok {
        Ok(())
    } else {
        Err(LifecycleError::Config(format!(
            "`{branch}` is not a branch name"
        )))
    }
}

/// Refuses a clone whose own config has a key in [`UNSAFE_KEYS`].
fn check_config(clone: &Path) -> Result<(), LifecycleError> {
    let found = git(
        Some(clone),
        &[
            "config",
            "--local",
            "--name-only",
            "--get-regexp",
            UNSAFE_KEYS,
        ],
        None,
    )
    .unwrap_or_default();
    match found.lines().next() {
        None => Ok(()),
        Some(key) => Err(LifecycleError::Config(format!(
            "{}: its git config sets `{key}`; remove it or make the clone again",
            clone.display()
        ))),
    }
}

/// The clone at `path` with `branch` of `source` up to date: cloned (bare)
/// when `path` does not exist, fetched otherwise. A `path` that exists but
/// is not a git repo is left alone and refused.
pub fn prepare(path: &Path, source: Source<'_>, branch: &str) -> Result<(), LifecycleError> {
    if path.exists() {
        return fetch(path, source, branch);
    }
    check_branch(branch)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| LifecycleError::io(parent, &e))?;
    }
    let shown = path.display().to_string();
    let cloned = git(
        None,
        &["clone", "--bare", "--no-tags", "--", source.url, &shown],
        source.token(),
    )
    .and_then(|_| crate::job::resolve(path, branch).map(drop));
    if cloned.is_err() {
        // Half a clone would be taken for one next time.
        let _ = std::fs::remove_dir_all(path);
    }
    cloned
}

/// [`prepare`] off the async runtime.
pub async fn prepare_async(
    path: &Path,
    url: &str,
    branch: &str,
    token: Option<&str>,
) -> Result<(), LifecycleError> {
    let (path, url, branch) = (path.to_owned(), url.to_owned(), branch.to_owned());
    let token = token.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        let source = Source {
            url: &url,
            token: token.as_deref(),
        };
        prepare(&path, source, &branch)
    })
    .await
    .map_err(|e| LifecycleError::Config(format!("cloning: {e}")))?
}

/// Fetches `branch` of `source` into the clone at `path`, moving the
/// clone's branch of that name to it, even if it was pushed over.
pub fn fetch(path: &Path, source: Source<'_>, branch: &str) -> Result<(), LifecycleError> {
    check_branch(branch)?;
    git(Some(path), &["rev-parse", "--git-dir"], None)
        .map_err(|_| LifecycleError::Config(format!("{}: not a git repo", path.display())))?;
    check_config(path)?;
    let refspec = format!("+refs/heads/{branch}:refs/heads/{branch}");
    // `--update-head-ok`: a clone made by hand has the branch checked out;
    // nothing reads its working tree.
    git(
        Some(path),
        &[
            "fetch",
            "--no-tags",
            "--update-head-ok",
            "--",
            source.url,
            &refspec,
        ],
        source.token(),
    )
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp(name: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("aegis-clone-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    /// An upstream repo with one commit on `main`; commits more with the
    /// returned function.
    fn upstream(dir: &Path) -> impl Fn(&str) -> String + '_ {
        let g = move |args: &[&str]| {
            git(
                Some(dir),
                &[
                    &["-c", "user.name=t", "-c", "user.email=t@example.com"],
                    args,
                ]
                .concat(),
                None,
            )
            .unwrap()
        };
        g(&["init", "-q", "-b", "main"]);
        let commit = move |text: &str| {
            std::fs::write(dir.join("a.txt"), text).unwrap();
            g(&["add", "-A"]);
            g(&["commit", "-qm", text]);
            g(&["rev-parse", "HEAD"])
        };
        commit("one");
        commit
    }

    fn source(dir: &Path) -> String {
        dir.display().to_string()
    }

    #[test]
    fn a_new_clone_is_made_then_kept_up_to_date() {
        let tmp = temp("fresh");
        let up = tmp.0.join("up");
        std::fs::create_dir_all(&up).unwrap();
        let commit = upstream(&up);
        let url = source(&up);
        let src = Source {
            url: &url,
            token: None,
        };
        let clone = tmp.0.join("state/repos/demo");
        prepare(&clone, src, "main").unwrap();
        let first = git(Some(&up), &["rev-parse", "HEAD"], None).unwrap();
        assert_eq!(crate::job::resolve(&clone, "main").unwrap(), first);

        let second = commit("two");
        prepare(&clone, src, "main").unwrap();
        assert_eq!(crate::job::resolve(&clone, "main").unwrap(), second);
    }

    #[test]
    fn a_clone_made_by_hand_with_the_branch_checked_out_is_fetched_too() {
        let tmp = temp("by-hand");
        let up = tmp.0.join("up");
        std::fs::create_dir_all(&up).unwrap();
        let commit = upstream(&up);
        let clone = tmp.0.join("clone");
        git(
            None,
            &["clone", "-q", &source(&up), &clone.display().to_string()],
            None,
        )
        .unwrap();
        let second = commit("two");
        let url = source(&up);
        fetch(
            &clone,
            Source {
                url: &url,
                token: None,
            },
            "main",
        )
        .unwrap();
        assert_eq!(crate::job::resolve(&clone, "main").unwrap(), second);
    }

    #[test]
    fn a_bad_url_or_branch_leaves_no_clone() {
        let tmp = temp("bad");
        let up = tmp.0.join("up");
        std::fs::create_dir_all(&up).unwrap();
        let _commit = upstream(&up);
        let url = source(&up);
        let clone = tmp.0.join("clone");
        for (url, branch) in [
            (url.as_str(), "no-such-branch"),
            ("/no/such/repo", "main"),
            (url.as_str(), "--upload-pack=x"),
            (url.as_str(), "main:refs/heads/x"),
        ] {
            let src = Source { url, token: None };
            assert!(prepare(&clone, src, branch).is_err(), "{url} {branch}");
            assert!(!clone.exists(), "{url} {branch}");
        }
    }

    #[test]
    fn a_clone_whose_config_could_redirect_the_fetch_is_refused() {
        let tmp = temp("unsafe");
        let up = tmp.0.join("up");
        std::fs::create_dir_all(&up).unwrap();
        let _commit = upstream(&up);
        let url = source(&up);
        let src = Source {
            url: &url,
            token: None,
        };
        let clone = tmp.0.join("clone");
        prepare(&clone, src, "main").unwrap();
        for (key, value) in [
            ("url.https://evil.example/.insteadOf", "https://github.com/"),
            ("http.extraHeader", "X: y"),
            ("core.sshCommand", "touch /tmp/x"),
        ] {
            git(Some(&clone), &["config", key, value], None).unwrap();
            let err = fetch(&clone, src, "main").unwrap_err().to_string();
            assert!(err.contains("its git config sets"), "{key}: {err}");
            git(Some(&clone), &["config", "--unset", key], None).unwrap();
        }
        fetch(&clone, src, "main").unwrap();
    }

    #[test]
    fn the_token_goes_over_https_only_and_is_not_shown() {
        let https = Source {
            url: "https://github.com/acme/app",
            token: Some("s3cret"),
        };
        assert_eq!(https.token(), Some("s3cret"));
        assert!(!format!("{https:?}").contains("s3cret"));
        for url in [
            "http://github.com/acme/app",
            "/srv/app.git",
            "git@github.com:a/b",
        ] {
            let other = Source {
                url,
                token: Some("s3cret"),
            };
            assert_eq!(other.token(), None, "{url}");
        }
    }
}
