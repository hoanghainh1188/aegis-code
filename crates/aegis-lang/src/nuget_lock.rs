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

/// Properties that add package sources or package folders.
const SOURCE_PROPERTIES: &[&str] = &[
    "RestoreSources",
    "RestoreAdditionalProjectSources",
    "RestoreFallbackFolders",
    "RestoreAdditionalProjectFallbackFolders",
    "RestorePackagesPath",
    "RestoreConfigFile",
];

/// MSBuild SDKs that ship with the .NET SDK; anything else is fetched from
/// NuGet and runs at build time.
fn builtin_sdk(name: &str) -> bool {
    name == "Microsoft.NET.Sdk" || name.starts_with("Microsoft.NET.Sdk.")
}

/// `Name/Version` or `Name` items of an `Sdk` attribute (`;`-separated).
fn sdks(value: &str) -> impl Iterator<Item = (String, String)> + '_ {
    value
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|sdk| {
            let (name, version) = sdk.split_once('/').unwrap_or((sdk, ""));
            let name = name.trim();
            (!builtin_sdk(name) || !version.is_empty())
                .then(|| (format!("sdk:{name}"), version.trim().to_owned()))
        })
}

/// MSBuild files (`*.csproj`, `*.fsproj`, `*.vbproj`, `*.proj`, `*.props`,
/// `*.targets`): `PackageReference` / `PackageVersion` /
/// `GlobalPackageReference` / `PackageDownload` items, non-built-in MSBuild
/// SDKs, and properties that add package sources (reported by value, so
/// any change is a new source). An `Include` that MSBuild expands
/// (`$(…)`, `@(…)`, wildcards, lists) could name anything: refused.
pub(crate) fn msbuild_project(file: &str, text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let doc = Document::parse(text.trim_start_matches('\u{feff}'))
        .map_err(|e| invalid(file, e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "Project" {
        return Err(invalid(file, "root element is not <Project>"));
    }
    let mut out = Vec::new();
    if let Some(sdk) = root.attribute("Sdk") {
        out.extend(sdks(sdk).map(|(n, v)| dep(&n, &v, NUGET)));
    }
    for node in doc.descendants().filter(|n| n.is_element()) {
        let tag = node.tag_name().name();
        let line = doc.text_pos_at(node.range().start).row;
        let err = |m: String| invalid(file, format!("line {line}: {m}"));
        match tag {
            "PackageReference"
            | "PackageVersion"
            | "GlobalPackageReference"
            | "PackageDownload" => {
                if node.attribute("Remove").is_some() && node.attribute("Include").is_none() {
                    continue;
                }
                let name = node
                    .attribute("Include")
                    .or_else(|| node.attribute("Update"))
                    .ok_or_else(|| err(format!("<{tag}> without Include")))?;
                if name.contains(['$', '@', '*', ';', '%']) {
                    return Err(err(format!(
                        "<{tag} Include=\"{name}\"> is expanded by MSBuild"
                    )));
                }
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
            "Sdk" => {
                let name = node
                    .attribute("Name")
                    .ok_or_else(|| err("<Sdk> without Name".into()))?;
                let version = node.attribute("Version").unwrap_or_default();
                let spec = if version.is_empty() {
                    name.to_owned()
                } else {
                    format!("{name}/{version}")
                };
                out.extend(sdks(&spec).map(|(n, v)| dep(&n, &v, NUGET)));
            }
            "Import" => {
                if let Some(sdk) = node.attribute("Sdk") {
                    out.extend(sdks(sdk).map(|(n, v)| dep(&n, &v, NUGET)));
                }
            }
            property if SOURCE_PROPERTIES.contains(&property) => {
                let value: String = node.children().filter_map(|c| c.text()).collect();
                out.push(dep(
                    &format!("msbuild:{property}"),
                    "",
                    &format!("value+{}", value.trim()),
                ));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// `global.json` `msbuild-sdks`: SDKs restored from NuGet.
pub(crate) fn global_json(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let value: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| invalid("global.json", e.to_string()))?;
    let Some(sdks) = value.get("msbuild-sdks") else {
        return Ok(Vec::new());
    };
    let sdks = sdks
        .as_object()
        .ok_or_else(|| invalid("global.json", "`msbuild-sdks` is not an object"))?;
    sdks.iter()
        .map(|(name, version)| {
            let version = version.as_str().ok_or_else(|| {
                invalid(
                    "global.json",
                    format!("version of `{name}` is not a string"),
                )
            })?;
            Ok(dep(&format!("sdk:{name}"), version, NUGET))
        })
        .collect()
}

#[derive(Deserialize)]
struct ToolsManifest {
    #[serde(default)]
    tools: BTreeMap<String, Tool>,
}

#[derive(Deserialize)]
struct Tool {
    version: String,
}

/// `.config/dotnet-tools.json`: local tools, NuGet packages run by
/// `dotnet tool restore`.
pub(crate) fn dotnet_tools(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let manifest: ToolsManifest =
        serde_json::from_str(text).map_err(|e| invalid("dotnet-tools.json", e.to_string()))?;
    Ok(manifest
        .tools
        .into_iter()
        .map(|(id, tool)| dep(&format!("tool:{id}"), &tool.version, NUGET))
        .collect())
}

/// FNV-1a, so a section's source changes whenever its text does.
fn fingerprint(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// `<config>` keys that point NuGet at packages already on disk.
const PACKAGE_FOLDER_KEYS: &[&str] = &["globalPackagesFolder", "repositoryPath"];

/// `nuget.config`: package sources other than nuget.org, as pseudo-
/// dependencies `nuget-source:<key>`. `packageSourceMapping` and
/// `disabledPackageSources` route or enable sources without adding one, so
/// each is reported by a fingerprint of its text. Settings that make NuGet
/// use packages already on disk are refused.
pub(crate) fn nuget_config(file: &str, text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let doc = Document::parse(text.trim_start_matches('\u{feff}'))
        .map_err(|e| invalid(file, e.to_string()))?;
    if doc.root_element().tag_name().name() != "configuration" {
        return Err(invalid(file, "root element is not <configuration>"));
    }
    let mut out = Vec::new();
    for section in doc.root_element().children().filter(|n| n.is_element()) {
        let line = doc.text_pos_at(section.range().start).row;
        let err = |m: String| invalid(file, format!("line {line}: {m}"));
        let adds = || section.children().filter(|n| n.tag_name().name() == "add");
        match section.tag_name().name() {
            "packageSources" => {
                for add in adds() {
                    let (Some(key), Some(url)) = (add.attribute("key"), add.attribute("value"))
                    else {
                        return Err(err("<add> needs key and value".into()));
                    };
                    let url = url.trim().trim_end_matches('/');
                    if url.eq_ignore_ascii_case(NUGET) {
                        continue;
                    }
                    out.push(dep(&format!("nuget-source:{key}"), "", url));
                }
            }
            "config" => {
                if let Some(key) = adds().filter_map(|a| a.attribute("key")).find(|k| {
                    PACKAGE_FOLDER_KEYS
                        .iter()
                        .any(|f| f.eq_ignore_ascii_case(k))
                }) {
                    return Err(err(format!("`{key}` is not allowed")));
                }
            }
            "fallbackPackageFolders" if adds().next().is_some() => {
                return Err(err("fallbackPackageFolders is not allowed".into()));
            }
            name @ ("packageSourceMapping" | "disabledPackageSources") => {
                let text = &doc.input_text()[section.range()];
                out.push(dep(
                    &format!("nuget-config:{name}"),
                    "",
                    &format!("content+{}", fingerprint(text)),
                ));
            }
            _ => {}
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

    fn names(deps: &[LockedDependency]) -> Vec<&str> {
        deps.iter().map(|d| d.name.as_str()).collect()
    }

    #[test]
    fn msbuild_sdks_downloads_and_source_properties_are_dependencies() {
        let project = r#"<Project Sdk="Microsoft.NET.Sdk;Evil.Sdk/1.0">
  <Sdk Name="Other.Sdk" Version="2.0" />
  <Import Project="x.props" Sdk="Third.Sdk/3.0" />
  <PropertyGroup><RestoreSources>https://evil.example/v3/index.json</RestoreSources></PropertyGroup>
  <ItemGroup><PackageDownload Include="Evil.Tool" Version="[1.0.0]" /></ItemGroup>
</Project>"#;
        let deps = msbuild_project("App.fsproj", project).unwrap();
        assert_eq!(
            names(&deps),
            vec![
                "sdk:evil.sdk",
                "sdk:other.sdk",
                "sdk:third.sdk",
                "msbuild:restoresources",
                "evil.tool"
            ]
        );
        let sources = deps
            .iter()
            .find(|d| d.name == "msbuild:restoresources")
            .unwrap();
        assert_eq!(sources.source, "value+https://evil.example/v3/index.json");
        assert!(
            msbuild_project("A.csproj", r#"<Project Sdk="Microsoft.NET.Sdk.Web" />"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn expanded_includes_are_refused() {
        for include in ["$(Pkg)", "@(Items)", "Evil.*", "A;B"] {
            let project = format!(
                r#"<Project><ItemGroup><PackageReference Include="{include}" Version="1" /></ItemGroup></Project>"#
            );
            let err = msbuild_project("A.csproj", &project)
                .unwrap_err()
                .to_string();
            assert!(err.contains("expanded by MSBuild"), "{include}: {err}");
        }
    }

    #[test]
    fn global_json_and_local_tools_are_dependencies() {
        let global = r#"{"sdk": {"version": "10.0.100"}, "msbuild-sdks": {"Microsoft.Build.NoTargets": "3.7.0"}}"#;
        assert_eq!(
            names(&global_json(global).unwrap()),
            vec!["sdk:microsoft.build.notargets"]
        );
        assert!(
            global_json(r#"{"sdk": {"version": "10.0.100"}}"#)
                .unwrap()
                .is_empty()
        );
        let tools = r#"{"version": 1, "isRoot": true, "tools": {"dotnet-ef": {"version": "9.0.0", "commands": ["dotnet-ef"]}}}"#;
        assert_eq!(names(&dotnet_tools(tools).unwrap()), vec!["tool:dotnet-ef"]);
    }

    #[test]
    fn nuget_config_package_folders_are_refused_and_routing_is_a_source() {
        let err = |t: &str| nuget_config("nuget.config", t).unwrap_err().to_string();
        assert!(err(r#"<configuration><config><add key="globalPackagesFolder" value="./pkgs" /></config></configuration>"#).contains("globalPackagesFolder"));
        assert!(err(r#"<configuration><fallbackPackageFolders><add key="x" value="./pkgs" /></fallbackPackageFolders></configuration>"#).contains("fallbackPackageFolders"));
        let mapping = |pattern: &str| {
            let text = format!(
                r#"<configuration><packageSourceMapping><packageSource key="nuget.org"><package pattern="{pattern}" /></packageSource></packageSourceMapping></configuration>"#
            );
            nuget_config("NuGet.config", &text).unwrap()
        };
        let a = mapping("*");
        let b = mapping("Newtonsoft.*");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "nuget-config:packagesourcemapping");
        assert_ne!(
            a[0].source, b[0].source,
            "changing the mapping changes the source"
        );
    }
}
