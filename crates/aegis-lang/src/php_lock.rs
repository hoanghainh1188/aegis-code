//! `composer.lock` reader (#37). Fails closed like the npm and Python
//! readers: packages from Packagist carry `notification-url` pointing at
//! `https://packagist.org/`, while custom VCS, Satis, or external path
//! repositories report their own URL and are blocked as unofficial sources.

use serde::Deserialize;

use crate::python_lock::in_repo;
use crate::{LangError, LockedDependency};

/// Official PHP package registry.
pub(crate) const PACKAGIST: &str = "https://packagist.org";

#[derive(Deserialize)]
struct ComposerLock {
    packages: Option<Vec<Package>>,
    #[serde(default, rename = "packages-dev")]
    packages_dev: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: Option<String>,
    version: Option<String>,
    #[serde(rename = "notification-url")]
    notification_url: Option<String>,
    source: Option<Location>,
    dist: Option<Location>,
}

#[derive(Deserialize)]
struct Location {
    #[serde(rename = "type")]
    kind: Option<String>,
    url: Option<String>,
}

fn invalid(message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: "composer.lock".into(),
        message: message.into(),
    }
}

/// Composer package names are `vendor/package` in lowercase ASCII.
fn valid_name(name: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-._".contains(&b))
    };
    name.split_once('/')
        .is_some_and(|(vendor, pkg)| part(vendor) && part(pkg))
}

pub(crate) fn composer_lock(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let lock: ComposerLock = serde_json::from_str(text).map_err(|e| invalid(e.to_string()))?;
    let packages = lock
        .packages
        .ok_or_else(|| invalid("missing `packages` array"))?;

    let mut out = Vec::with_capacity(packages.len() + lock.packages_dev.len());
    for pkg in packages.into_iter().chain(lock.packages_dev) {
        let name = pkg
            .name
            .filter(|n| valid_name(n))
            .ok_or_else(|| invalid("package missing a valid `vendor/name`"))?;
        let version = pkg
            .version
            .filter(|v| !v.is_empty())
            .ok_or_else(|| invalid(format!("package `{name}` missing `version`")))?;

        let is_local_path = [&pkg.dist, &pkg.source].into_iter().flatten().any(|loc| {
            loc.kind.as_deref() == Some("path") && loc.url.as_deref().is_some_and(in_repo)
        });
        if is_local_path {
            continue;
        }

        let source = if pkg
            .notification_url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://packagist.org/"))
        {
            PACKAGIST.to_owned()
        } else if let Some(url) = pkg
            .dist
            .as_ref()
            .and_then(|d| d.url.as_deref())
            .or_else(|| pkg.source.as_ref().and_then(|s| s.url.as_deref()))
            .filter(|u| !u.is_empty())
        {
            url.to_owned()
        } else {
            return Err(invalid(format!(
                "package `{name}` has no Packagist notification-url, dist, or source"
            )));
        };

        out.push(LockedDependency {
            name,
            version,
            source,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_packagist_packages_and_dev_packages() {
        let lock = r#"{
            "packages": [
                {
                    "name": "symfony/console",
                    "version": "v7.1.0",
                    "notification-url": "https://packagist.org/downloads/",
                    "source": { "type": "git", "url": "https://github.com/symfony/console.git" },
                    "dist": { "type": "zip", "url": "https://api.github.com/repos/symfony/console/zipball/abc" }
                }
            ],
            "packages-dev": [
                {
                    "name": "phpunit/phpunit",
                    "version": "11.3.1",
                    "notification-url": "https://packagist.org/downloads/",
                    "dist": { "type": "zip", "url": "https://api.github.com/repos/sebastianbergmann/phpunit/zipball/def" }
                }
            ]
        }"#;
        let deps = composer_lock(lock).unwrap();
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "symfony/console");
        assert_eq!(deps[0].version, "v7.1.0");
        assert_eq!(deps[0].source, PACKAGIST);
        assert_eq!(deps[1].name, "phpunit/phpunit");
        assert_eq!(deps[1].source, PACKAGIST);
    }

    #[test]
    fn custom_vcs_and_external_paths_keep_their_own_source() {
        let lock = r#"{
            "packages": [
                {
                    "name": "acme/internal",
                    "version": "1.0.0",
                    "source": { "type": "git", "url": "https://git.evil.example/acme/internal.git" }
                },
                {
                    "name": "acme/local",
                    "version": "dev-main",
                    "dist": { "type": "path", "url": "packages/local" }
                }
            ]
        }"#;
        let deps = composer_lock(lock).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "acme/internal");
        assert_eq!(deps[0].source, "https://git.evil.example/acme/internal.git");
    }

    #[test]
    fn fails_closed_on_malformed_or_missing_fields() {
        assert!(composer_lock("not json").is_err());
        assert!(composer_lock("{}").is_err());
        assert!(composer_lock(r#"{"packages":[{"name":"badname","version":"1.0"}]}"#).is_err());
        assert!(composer_lock(r#"{"packages":[{"name":"a/b","version":"1.0"}]}"#).is_err());
    }
}
