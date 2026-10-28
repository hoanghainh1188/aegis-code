//! Secrets as files, one per name, in a directory (#261): Docker secrets
//! (`/run/secrets`), a Kubernetes Secret mounted as a volume, systemd's
//! `LoadCredential=`. The way an on-premise deployment, without Secrets
//! Manager, hands secrets to a process.
//!
//! A file is read every time the secret is: a new file is a rotated
//! secret, without a restart. A file anyone but its owner may read or
//! write, or a link that leads out of the directory, is refused. The
//! directory itself is not checked: Docker's and Kubernetes' are `0755`
//! and owned by root, and names are not secret.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::source::{Fetch, SecretSource};

#[derive(Debug, Clone)]
pub struct FileSource {
    /// Canonical: links are judged against it.
    dir: PathBuf,
}

impl FileSource {
    /// Fails when `dir` is not a directory.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let shown = dir.display();
        let real = dir
            .canonicalize()
            .map_err(|e| format!("secret directory `{shown}`: {e}"))?;
        if !real.is_dir() {
            return Err(format!("secret directory `{shown}` is not a directory"));
        }
        Ok(Self { dir: real })
    }

    fn read(&self, name: &str) -> Result<Option<String>, String> {
        // A name is a file of the directory, never a path.
        if name.is_empty()
            || name.starts_with('.')
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Err(format!("`{name}` is not a secret file name"));
        }
        let path = self.dir.join(name);
        let real = match path.canonicalize() {
            Ok(real) => real,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("secret `{name}`: {e}")),
        };
        if !real.starts_with(&self.dir) {
            return Err(format!(
                "secret `{name}` links out of {}",
                self.dir.display()
            ));
        }
        let meta = std::fs::metadata(&real).map_err(|e| format!("secret `{name}`: {e}"))?;
        if !meta.is_file() {
            return Err(format!("secret `{name}` is not a file"));
        }
        check_mode(name, &meta)?;
        let text = std::fs::read_to_string(&real).map_err(|e| format!("secret `{name}`: {e}"))?;
        Ok(Some(text.trim_end().to_owned()))
    }
}

/// Only the owner may read or write a secret file.
#[cfg(unix)]
fn check_mode(name: &str, meta: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "secret `{name}` is open to others than its owner (mode {mode:o}): make it 0400 or 0600"
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_mode(_name: &str, _meta: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

impl SecretSource for FileSource {
    fn describe(&self) -> String {
        format!("files in {}", self.dir.display())
    }

    fn fetch<'a>(&'a self, name: &'a str) -> Fetch<'a> {
        let value = self.read(name);
        Box::pin(async move { value })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;
    use crate::Secrets;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("aegis-secrets-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, name: &str, text: &str, mode: u32) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn a_file_is_read_trimmed_and_again_when_it_changes() {
        let dir = Dir::new("rotate");
        dir.write("AEGIS_DATABASE_URL", "postgres://a\n", 0o400);
        let secrets = Secrets::new(FileSource::open(&dir.0).unwrap());
        secrets.require(&["AEGIS_DATABASE_URL"]).await.unwrap();
        assert_eq!(
            secrets.get("AEGIS_DATABASE_URL").unwrap().expose(),
            "postgres://a"
        );
        std::fs::remove_file(dir.0.join("AEGIS_DATABASE_URL")).unwrap();
        dir.write("AEGIS_DATABASE_URL", "postgres://b \n", 0o600);
        assert_eq!(secrets.refresh().await, ["AEGIS_DATABASE_URL"]);
        assert_eq!(
            secrets.get("AEGIS_DATABASE_URL").unwrap().expose(),
            "postgres://b"
        );
    }

    #[tokio::test]
    async fn a_missing_file_is_a_missing_secret() {
        let dir = Dir::new("missing");
        let secrets = Secrets::new(FileSource::open(&dir.0).unwrap());
        let err = secrets.require(&["AEGIS_SESSION_KEY"]).await.unwrap_err();
        assert!(err.to_string().contains("AEGIS_SESSION_KEY"), "{err}");
    }

    #[tokio::test]
    async fn a_file_others_may_read_is_refused_without_its_value() {
        let dir = Dir::new("mode");
        dir.write("GITHUB_TOKEN", "ghp_very_secret", 0o644);
        let secrets = Secrets::new(FileSource::open(&dir.0).unwrap());
        let err = secrets.require(&["GITHUB_TOKEN"]).await.unwrap_err();
        let err = err.to_string();
        assert!(err.contains("mode 644"), "{err}");
        assert!(!err.contains("ghp_very_secret"), "{err}");
    }

    #[tokio::test]
    async fn links_stay_inside_the_directory() {
        let dir = Dir::new("links");
        let outside = Dir::new("links-outside");
        let target = outside.write("token", "x", 0o400);
        symlink(&target, dir.0.join("OUT")).unwrap();
        // As a Kubernetes Secret volume lays out its files.
        std::fs::create_dir(dir.0.join("..data")).unwrap();
        dir.write("..data/IN", "inside", 0o400);
        symlink("..data/IN", dir.0.join("IN")).unwrap();
        let source = FileSource::open(&dir.0).unwrap();
        let err = source.fetch("OUT").await.unwrap_err();
        assert!(err.contains("links out of"), "{err}");
        assert_eq!(source.fetch("IN").await, Ok(Some("inside".into())));
    }

    #[tokio::test]
    async fn names_are_file_names_not_paths() {
        let dir = Dir::new("names");
        let source = FileSource::open(&dir.0).unwrap();
        for name in ["../etc/passwd", "a/b", ".hidden", ""] {
            assert!(source.fetch(name).await.is_err(), "{name}");
        }
        assert!(FileSource::open(&dir.0.join("none")).is_err());
    }
}
