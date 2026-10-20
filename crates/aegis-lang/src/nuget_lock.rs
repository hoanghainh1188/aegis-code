//! .NET dependency readers (#70). NuGet ids compare case-insensitively, so
//! names are lower-cased.
//!
//! - `packages.lock.json` (with `RestorePackagesWithLockFile`): every
//!   package of every target framework; in-repo projects are skipped.
//! - `*.csproj`, `Directory.Packages.props`: `PackageReference` /
//!   `PackageVersion` items, for repos without a lockfile (direct only;
//!   the resolved graph is #80).
//! - `nuget.config`: where packages come from. Lockfiles do not record the
//!   feed, so every package source other than nuget.org is reported as a
//!   pseudo-dependency `nuget-source:<key>` from that URL; adding one is an
//!   unofficial source.
//!
//! Fail closed on anything they cannot read.

use std::collections::BTreeMap;

use roxmltree::Document;
use serde::Deserialize;

use crate::{LangError, LockedDependency};

/// The nuget.org v3 feed, the official source.
pub(crate) const NUGET: &str = "https://api.nuget.org/v3/index.json";

fn invalid(file: &str, message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: file.to_owned(),
        message: message.into(),
    }
}

fn dep(name: &str, version: &str, source: &str) -> LockedDependency {
    LockedDependency {
        name: name.to_ascii_lowercase(),
        version: version.to_owned(),
        source: source.to_owned(),
    }
}

#[derive(Deserialize)]
struct PackagesLock {
    version: u32,
    dependencies: BTreeMap<String, BTreeMap<String, LockEntry>>,
}

#[derive(Deserialize)]
struct LockEntry {
    #[serde(rename = "type")]
    kind: String,
    resolved: Option<String>,
}

pub(crate) fn packages_lock(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let file = "packages.lock.json";
    let lock: PackagesLock =
        serde_json::from_str(text).map_err(|e| invalid(file, e.to_string()))?;
    if !(1..=2).contains(&lock.version) {
        return Err(invalid(
            file,
            format!("lockfile version {} is not supported", lock.version),
        ));
    }
    let mut out = BTreeMap::new();
    for (framework, packages) in lock.dependencies {
        for (name, entry) in packages {
            let err = |m: &str| invalid(file, format!("{framework} `{name}`: {m}"));
            match entry.kind.as_str() {
                "Project" => continue,
                "Direct" | "Transitive" | "CentralTransitive" => {}
                other => return Err(err(&format!("unknown type `{other}`"))),
            }
            let version = entry.resolved.ok_or_else(|| err("no `resolved` version"))?;
            let d = dep(&name, &version, NUGET);
            out.insert((d.name.clone(), d.version.clone()), d);
        }
    }
    Ok(out.into_values().collect())
}

/// `PackageReference Include="X" Version="1"` (also `Update`, and the
/// version as a child element) and central `PackageVersion` items.
pub(crate) fn msbuild_project(file: &str, text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let doc = Document::parse(text.trim_start_matches('\u{feff}'))
        .map_err(|e| invalid(file, e.to_string()))?;
    if doc.root_element().tag_name().name() != "Project" {
        return Err(invalid(file, "root element is not <Project>"));
    }
    let mut out = Vec::new();
    for node in doc.descendants().filter(|n| n.is_element()) {
        let tag = node.tag_name().name();
        if !matches!(
            tag,
            "PackageReference" | "PackageVersion" | "GlobalPackageReference"
        ) {
            continue;
        }
        let line = doc.text_pos_at(node.range().start).row;
        let name = node
            .attribute("Include")
            .or_else(|| node.attribute("Update"))
            .ok_or_else(|| invalid(file, format!("line {line}: <{tag}> without Include")))?;
        let version = node
            .attribute("Version")
            .or_else(|| node.attribute("VersionOverride"))
            .or_else(|| {
                node.children()
                    .find(|c| c.tag_name().name() == "Version")
                    .and_then(|c| c.text())
            })
            .unwrap_or_default()
            .trim();
        out.push(dep(name, version, NUGET));
    }
    Ok(out)
}

/// Package sources in `nuget.config` other than nuget.org.
pub(crate) fn nuget_config(file: &str, text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let doc = Document::parse(text.trim_start_matches('\u{feff}'))
        .map_err(|e| invalid(file, e.to_string()))?;
    if doc.root_element().tag_name().name() != "configuration" {
        return Err(invalid(file, "root element is not <configuration>"));
    }
    let mut out = Vec::new();
    for sources in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "packageSources")
    {
        for add in sources.children().filter(|n| n.tag_name().name() == "add") {
            let line = doc.text_pos_at(add.range().start).row;
            let (Some(key), Some(url)) = (add.attribute("key"), add.attribute("value")) else {
                return Err(invalid(
                    file,
                    format!("line {line}: <add> needs key and value"),
                ));
            };
            let url = url.trim().trim_end_matches('/');
            if url.eq_ignore_ascii_case(NUGET) {
                continue;
            }
            out.push(dep(&format!("nuget-source:{key}"), "", url));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_packages_lock() {
        let text = r#"{
  "version": 1,
  "dependencies": {
    "net10.0": {
      "xunit": {"type": "Direct", "requested": "[2.9.3, )", "resolved": "2.9.3", "contentHash": "x"},
      "Microsoft.CodeCoverage": {"type": "Transitive", "resolved": "17.14.1", "contentHash": "x"},
      "cart": {"type": "Project"}
    },
    "net8.0": {
      "xunit": {"type": "Direct", "resolved": "2.9.3", "contentHash": "x"}
    }
  }
}"#;
        let deps = packages_lock(text).unwrap();
        assert_eq!(
            deps,
            vec![
                dep("Microsoft.CodeCoverage", "17.14.1", NUGET),
                dep("xunit", "2.9.3", NUGET)
            ]
        );
        assert_eq!(deps[0].name, "microsoft.codecoverage");
    }

    #[test]
    fn packages_lock_fails_closed() {
        let err = |t: &str| packages_lock(t).unwrap_err().to_string();
        assert!(err(r#"{"version": 9, "dependencies": {}}"#).contains("version 9"));
        assert!(err(r#"{"version": 1, "dependencies": {"net10.0": {"x": {"type": "Odd", "resolved": "1"}}}}"#).contains("unknown type"));
        assert!(
            err(r#"{"version": 1, "dependencies": {"net10.0": {"x": {"type": "Direct"}}}}"#)
                .contains("no `resolved`")
        );
    }

    #[test]
    fn reads_package_references() {
        let csproj = "\u{feff}<Project Sdk=\"Microsoft.NET.Sdk\">
  <ItemGroup>
    <PackageReference Include=\"Newtonsoft.Json\" Version=\"13.0.3\" />
    <PackageReference Include=\"Serilog\"><Version>4.1.0</Version></PackageReference>
    <PackageReference Include=\"Central.Pkg\" />
    <ProjectReference Include=\"..\\lib\\lib.csproj\" />
  </ItemGroup>
</Project>";
        let deps = msbuild_project("App.csproj", csproj).unwrap();
        assert_eq!(
            deps,
            vec![
                dep("Newtonsoft.Json", "13.0.3", NUGET),
                dep("Serilog", "4.1.0", NUGET),
                dep("Central.Pkg", "", NUGET),
            ]
        );
        let props = "<Project><ItemGroup><PackageVersion Include=\"Central.Pkg\" Version=\"1.2.0\" /></ItemGroup></Project>";
        assert_eq!(
            msbuild_project("Directory.Packages.props", props).unwrap()[0].version,
            "1.2.0"
        );
        assert!(
            msbuild_project(
                "A.csproj",
                "<Project><ItemGroup><PackageReference Version=\"1\"/></ItemGroup></Project>"
            )
            .is_err()
        );
        assert!(msbuild_project("A.csproj", "<NotProject/>").is_err());
    }

    #[test]
    fn nuget_config_reports_unofficial_sources() {
        let config = r#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />
    <add key="evil" value="https://evil.example/v3/index.json/" />
  </packageSources>
</configuration>"#;
        assert_eq!(
            nuget_config("nuget.config", config).unwrap(),
            vec![dep(
                "nuget-source:evil",
                "",
                "https://evil.example/v3/index.json"
            )]
        );
        assert!(
            nuget_config(
                "nuget.config",
                "<configuration><packageSources><add key=\"x\"/></packageSources></configuration>"
            )
            .is_err()
        );
        assert!(nuget_config("nuget.config", "<settings/>").is_err());
    }
}
