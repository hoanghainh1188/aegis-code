//! Commits exactly the tree the gate checked and pushes it to the agent
//! branch.
//!
//! Nothing of the repo's own git config is read: the commit and the push
//! run in a fresh git dir that only borrows the repo's objects
//! (`objects/info/alternates`), with no global or system config. So a
//! `url.*.insteadOf`, proxy, extra header or credential helper planted in
//! the repo cannot redirect the push or see the token. The token goes in
//! an HTTP header through git's environment, never in argv or a URL, and
//! only an `agent/*` ref is ever pushed.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::{AgentBranch, PrError};

/// A push or delete still running after this long is killed.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(120);

/// Kept from this process: proxies and CA settings a network may need.
const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// Name and email on the agent's commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

/// Where the agent's branch is pushed: the fork over HTTPS, or a local
/// bare repo (tests, dry runs). Built only through the checked
/// constructors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    url: String,
    token: Option<String>,
}

fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

fn plain_host(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-.".contains(c))
        && !s.starts_with(['-', '.'])
}

impl Remote {
    /// `https://<host>/<owner>/<repo>.git`, pushed with `token`.
    pub fn https(host: &str, owner: &str, repo: &str, token: &str) -> Result<Self, PrError> {
        if !plain_host(host) || !plain_name(owner) || !plain_name(repo) {
            return Err(PrError::InvalidRemote(format!("{host}/{owner}/{repo}")));
        }
        Ok(Self {
            url: format!("https://{host}/{owner}/{repo}.git"),
            token: Some(token.to_owned()),
        })
    }

    /// A bare repo on this machine; no token is sent.
    pub fn local(path: &Path) -> Result<Self, PrError> {
        let path = path
            .canonicalize()
            .map_err(|e| PrError::InvalidRemote(format!("{}: {e}", path.display())))?;
        Ok(Self {
            url: format!("file://{}", path.display()),
            token: None,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A git dir of our own that sees the repo's objects and nothing else.
pub(crate) struct Staging {
    dir: PathBuf,
}

impl Staging {
    pub fn new(repo: &Path) -> Result<Self, PrError> {
        let objects = {
            let out = Command::new("git")
                .env_clear()
                .envs(std::env::var_os("PATH").map(|p| ("PATH", p)))
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(repo)
                .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .output()
                .map_err(|e| git_error("rev-parse", e.to_string()))?;
            if !out.status.success() {
                return Err(git_error("rev-parse", stderr_of(&out.stderr)));
            }
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).join("objects")
        };
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aegis-pr-staging-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let staging = Self { dir };
        staging.run(&["init", "--quiet", "--bare"], None, None)?;
        let alternates = staging.dir.join("objects/info/alternates");
        std::fs::write(&alternates, format!("{}\n", objects.display()))
            .map_err(|e| git_error("alternates", e.to_string()))?;
        Ok(staging)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.env_clear()
            .envs(
                INHERITED_ENV
                    .iter()
                    .filter_map(|k| Some((*k, std::env::var_os(k)?))),
            )
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_DIR", &self.dir)
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args);
        c
    }

    /// Runs git here; `stdin` is fed to it, `token` becomes an
    /// `Authorization` header for this call only.
    fn run(
        &self,
        args: &[&str],
        stdin: Option<&str>,
        token: Option<&str>,
    ) -> Result<String, PrError> {
        let what = args.first().copied().unwrap_or("git");
        let mut c = self.command(args);
        if let Some(token) = token {
            let basic = base64(format!("x-access-token:{token}").as_bytes());
            c.env("GIT_CONFIG_COUNT", "2")
                .env("GIT_CONFIG_KEY_0", "http.extraHeader")
                .env(
                    "GIT_CONFIG_VALUE_0",
                    format!("Authorization: Basic {basic}"),
                )
                .env("GIT_CONFIG_KEY_1", "credential.helper")
                .env("GIT_CONFIG_VALUE_1", "");
        }
        let mut child = c
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| git_error(what, e.to_string()))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // git may exit early; its stderr then says why.
            let _ = pipe.write_all(text.as_bytes());
        }
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| git_error(what, e.to_string()))?
            {
                break status;
            }
            if started.elapsed() > NETWORK_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(git_error(what, "timed out".into()));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let read = |pipe: Option<&mut dyn Read>| {
            let mut buf = Vec::new();
            if let Some(p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        };
        let stdout = read(child.stdout.as_mut().map(|p| p as &mut dyn Read));
        let stderr = read(child.stderr.as_mut().map(|p| p as &mut dyn Read));
        if status.success() {
            Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
        } else {
            Err(git_error(what, stderr_of(&stderr)))
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stderr_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

/// Errors name the git command only, never its arguments (the URL).
fn git_error(what: &str, message: String) -> PrError {
    PrError::Git {
        args: what.to_owned(),
        message,
    }
}

/// A commit of `tree` on top of `parent`, with `message`.
pub fn commit_tree(
    staging: &Staging,
    tree: &str,
    parent: &str,
    message: &str,
    who: &Identity,
) -> Result<String, PrError> {
    let mut args = vec!["commit-tree", tree, "-p", parent, "-F", "-"];
    let name = format!("user.name={}", who.name);
    let email = format!("user.email={}", who.email);
    let mut prefix = vec!["-c", name.as_str(), "-c", email.as_str()];
    prefix.append(&mut args);
    staging.run(&prefix, Some(message), None)
}

/// Pushes `commit` to `branch`. Fails if the branch already exists there
/// with other commits.
pub fn push(
    staging: &Staging,
    remote: &Remote,
    branch: &AgentBranch,
    commit: &str,
) -> Result<(), PrError> {
    push_ref(staging, remote, &branch.full_ref(), commit)
}

/// `commit` to `full_ref`; callers only pass `agent/*` and `eval/*` refs.
pub fn push_ref(
    staging: &Staging,
    remote: &Remote,
    full_ref: &str,
    commit: &str,
) -> Result<(), PrError> {
    let refspec = format!("{commit}:{full_ref}");
    let args = ["push", "--porcelain", remote.url.as_str(), refspec.as_str()];
    staging.run(&args, None, remote.token.as_deref()).map(drop)
}

/// Removes `branch` only while it still points at `commit`, so a branch a
/// person has since pushed to is left alone.
pub fn delete(
    staging: &Staging,
    remote: &Remote,
    branch: &AgentBranch,
    commit: &str,
) -> Result<(), PrError> {
    let full = branch.full_ref();
    let lease = format!("--force-with-lease={full}:{commit}");
    let refspec = format!(":{full}");
    let args = [
        "push",
        "--porcelain",
        lease.as_str(),
        remote.url.as_str(),
        refspec.as_str(),
    ];
    staging.run(&args, None, remote.token.as_deref()).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"x-access-token:abc"), "eC1hY2Nlc3MtdG9rZW46YWJj");
    }

    #[test]
    fn remotes_are_https_to_a_plain_repo_name() {
        let r = Remote::https("github.com", "me", "typst", "t").unwrap();
        assert_eq!(r.url(), "https://github.com/me/typst.git");
        for (host, owner, repo) in [
            ("github.com", "me/../x", "typst"),
            ("github.com", "--exec=x", "typst"),
            ("evil.com/github.com", "me", "typst"),
            ("", "me", "typst"),
            ("github.com", "me", ".."),
        ] {
            assert!(
                Remote::https(host, owner, repo, "t").is_err(),
                "{host}/{owner}/{repo}"
            );
        }
    }
}
