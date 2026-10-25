//! The audit step's policy (#159): the platform's defaults in `policies/`,
//! with a repo's own licenses and ignored advisories on top. The gate
//! writes the files `{deny_config}` and `{osv_config}` point to from here.

use serde::Deserialize;

use crate::LangError;

const LICENSES: &str = include_str!("../../../policies/licenses.toml");
const DENY: &str = include_str!("../../../policies/deny.toml");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Licenses {
    allowed: Vec<String>,
}

/// Licenses a repo's dependencies may have when it names none:
/// `policies/licenses.toml`.
pub fn default_licenses() -> Vec<String> {
    toml::from_str::<Licenses>(LICENSES)
        .expect("policies/licenses.toml is checked by the tests")
        .allowed
}

/// A license name for `--licenses=a,b`: an SPDX identifier, so no comma,
/// space or leading `-` can change the command.
pub fn check_license(name: &str) -> Result<(), String> {
    check_id(name, "-.+").map_err(|why| format!("license `{name}` {why}"))
}

/// An advisory id (`RUSTSEC-2024-0001`, `GHSA-xxxx-xxxx-xxxx`, `PYSEC-…`).
pub fn check_advisory(id: &str) -> Result<(), String> {
    check_id(id, "-_.:").map_err(|why| format!("advisory `{id}` {why}"))
}

fn check_id(s: &str, extra: &str) -> Result<(), &'static str> {
    if !s.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        Err("must start with a letter or digit")
    } else if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    {
        Err("has characters an identifier does not")
    } else {
        Ok(())
    }
}

/// `policies/deny.toml` with `licenses.allow` replaced by `licenses` when
/// the repo has its own, and `ignored` added to `advisories.ignore`.
pub fn deny_config(licenses: Option<&[String]>, ignored: &[String]) -> Result<String, LangError> {
    let invalid = |m: String| LangError::InvalidCommand(format!("deny config: {m}"));
    let mut config: toml::Table = toml::from_str(DENY).map_err(|e| invalid(e.to_string()))?;
    let section = |config: &mut toml::Table, name: &str| -> Result<toml::Table, LangError> {
        match config.remove(name) {
            Some(toml::Value::Table(t)) => Ok(t),
            _ => Err(invalid(format!("policies/deny.toml has no [{name}]"))),
        }
    };
    let strings = |v: &[String]| toml::Value::Array(v.iter().cloned().map(Into::into).collect());
    let mut license_section = section(&mut config, "licenses")?;
    if let Some(licenses) = licenses {
        license_section.insert("allow".into(), strings(licenses));
    }
    config.insert("licenses".into(), license_section.into());
    let mut advisories = section(&mut config, "advisories")?;
    let mut ignore = match advisories.remove("ignore") {
        Some(toml::Value::Array(a)) => a,
        _ => vec![],
    };
    ignore.extend(ignored.iter().cloned().map(toml::Value::from));
    advisories.insert("ignore".into(), ignore.into());
    config.insert("advisories".into(), advisories.into());
    toml::to_string(&config).map_err(|e| invalid(e.to_string()))
}

/// An `osv-scanner` configuration that ignores `ignored`.
pub fn osv_config(ignored: &[String]) -> Result<String, LangError> {
    let vulns: Vec<toml::Value> = ignored
        .iter()
        .map(|id| {
            let mut t = toml::Table::new();
            t.insert("id".into(), id.clone().into());
            t.insert("reason".into(), "ignored in the repo's settings".into());
            t.into()
        })
        .collect();
    let mut config = toml::Table::new();
    config.insert("IgnoredVulns".into(), vulns.into());
    toml::to_string(&config)
        .map_err(|e| LangError::InvalidCommand(format!("osv-scanner config: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn strings(t: &toml::Table, section: &str, key: &str) -> Vec<String> {
        t[section][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn the_default_licenses_are_policies_licenses_toml() {
        let licenses = default_licenses();
        assert!(licenses.contains(&"MIT".to_owned()));
        assert!(licenses.iter().all(|l| check_license(l).is_ok()));
    }

    #[test]
    fn the_deny_config_keeps_the_platform_policy_without_repo_settings() {
        let text = deny_config(None, &[]).unwrap();
        let config: toml::Table = toml::from_str(&text).unwrap();
        let platform: toml::Table = toml::from_str(DENY).unwrap();
        assert_eq!(config, platform);
    }

    #[test]
    fn the_deny_config_takes_the_repo_licenses_and_ignored_advisories() {
        let text = deny_config(
            Some(&names(&["MIT"])),
            &names(&["RUSTSEC-2024-0001", "RUSTSEC-2023-0071"]),
        )
        .unwrap();
        let config: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(strings(&config, "licenses", "allow"), ["MIT"]);
        assert_eq!(
            strings(&config, "advisories", "ignore"),
            ["RUSTSEC-2024-0001", "RUSTSEC-2023-0071"]
        );
        // The rest of the policy stays.
        assert_eq!(config["advisories"]["yanked"].as_str(), Some("deny"));
        assert_eq!(config["sources"]["unknown-git"].as_str(), Some("deny"));
    }

    #[test]
    fn the_osv_config_ignores_the_repo_advisories() {
        let text = osv_config(&names(&["GHSA-aaaa-bbbb-cccc", "PYSEC-2024-1"])).unwrap();
        let config: toml::Table = toml::from_str(&text).unwrap();
        let ids: Vec<&str> = config["IgnoredVulns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["GHSA-aaaa-bbbb-cccc", "PYSEC-2024-1"]);
        let empty: toml::Table = toml::from_str(&osv_config(&[]).unwrap()).unwrap();
        assert_eq!(empty["IgnoredVulns"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn licenses_and_advisories_must_be_plain_identifiers() {
        for ok in ["MIT", "Apache-2.0", "GPL-2.0+", "LicenseRef-x.1"] {
            assert!(check_license(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "MIT,GPL-3.0", "MIT OR GPL", "a\nb"] {
            assert!(check_license(bad).is_err(), "{bad:?}");
        }
        assert!(check_advisory("RUSTSEC-2024-0001").is_ok());
        assert!(check_advisory("GHSA-aaaa-bbbb-cccc").is_ok());
        for bad in ["", "--all", "a b", "x\"y"] {
            assert!(check_advisory(bad).is_err(), "{bad:?}");
        }
    }
}
