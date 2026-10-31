//! Where `aegis` keeps its login: the server and the person's token,
//! in a file only its owner can read. `AEGIS_SERVER` and
//! `AEGIS_TOKEN` stand in for it (CI, scripts).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SERVER_ENV: &str = "AEGIS_SERVER";
pub const TOKEN_ENV: &str = "AEGIS_TOKEN";
/// Another file than the default.
pub const CONFIG_ENV: &str = "AEGIS_CONFIG";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    /// `https://aegis.example.com`, without a trailing `/`.
    pub server: String,
    pub token: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("server", &self.server)
            .field("token", &"…")
            .finish()
    }
}

/// `$AEGIS_CONFIG`, or `cli.toml` under `$XDG_CONFIG_HOME/aegis` or
/// `~/.config/aegis`.
pub fn path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os(CONFIG_ENV) {
        return Ok(PathBuf::from(p));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or("no HOME to keep the login in; set AEGIS_CONFIG")?;
    Ok(base.join("aegis").join("cli.toml"))
}

/// A server URL the token may go to: `https://`, or `http://` to this
/// machine (a local trial). The token is never sent in clear elsewhere.
pub fn checked_server(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let local = ["http://localhost", "http://127.0.0.1", "http://[::1]"]
        .iter()
        .any(|p| {
            url == *p || url.starts_with(&format!("{p}:")) || url.starts_with(&format!("{p}/"))
        });
    if url.starts_with("https://") || local {
        Ok(url.to_owned())
    } else {
        Err(format!(
            "`{url}`: the server must be https:// (http:// only on this machine)"
        ))
    }
}

/// The login: from the environment when both are set, else the file.
pub fn load() -> Result<Login, String> {
    if let (Ok(server), Ok(token)) = (std::env::var(SERVER_ENV), std::env::var(TOKEN_ENV)) {
        return Ok(Login {
            server: checked_server(&server)?,
            token,
        });
    }
    let path = path()?;
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!("not logged in: `aegis login <server>` (or {SERVER_ENV} and {TOKEN_ENV})")
    })?;
    let login: Login = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Login {
        server: checked_server(&login.server)?,
        ..login
    })
}

/// Writes `login` to `path`, readable by its owner only.
pub fn save(path: &Path, login: &Login) -> Result<(), String> {
    let shown = path.display();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = toml::to_string(login).map_err(|e| e.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| format!("{shown}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // An older file keeps its mode on open: set it again.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{shown}: {e}"))?;
    }
    std::io::Write::write_all(&mut file, text.as_bytes()).map_err(|e| format!("{shown}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_goes_only_over_https_or_to_this_machine() {
        assert_eq!(
            checked_server("https://aegis.example.com/").unwrap(),
            "https://aegis.example.com"
        );
        assert!(checked_server("http://localhost:8080").is_ok());
        assert!(checked_server("http://127.0.0.1").is_ok());
        for bad in [
            "http://aegis.example.com",
            "http://localhost.evil.com",
            "ftp://x",
            "aegis.example.com",
        ] {
            assert!(checked_server(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_login_file_is_its_owners_only_and_hides_the_token_when_shown() {
        let dir = std::env::temp_dir().join(format!("aegis-config-{}", std::process::id()));
        let path = dir.join("cli.toml");
        let login = Login {
            server: "https://aegis.example.com".into(),
            token: "s3cret".into(),
        };
        save(&path, &login).unwrap();
        let back: Login = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, login);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(!format!("{login:?}").contains("s3cret"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
