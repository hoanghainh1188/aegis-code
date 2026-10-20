//! Java dependency readers (#68): `pom.xml` and `gradle.lockfile`.
//!
//! Maven has no lockfile, so `pom.xml` gives what the repo declares: every
//! `dependency`, `plugin` and build `extension` anywhere in the file
//! (dependency management, Maven profiles, plugin dependencies) and the
//! parent, since plugins and extensions are code that runs at build time.
//! Extra `<repositories>` change every coordinate's source, like extra
//! indexes for Python. Transitive dependencies need a resolved tree from
//! Maven itself (#80). Fails closed on what it cannot read.

use std::collections::BTreeSet;

use roxmltree::{Document, Node};

use crate::{LangError, LockedDependency};

/// Maven Central, the official repository.
pub(crate) const MAVEN_CENTRAL: &str = "https://repo.maven.apache.org/maven2";
const DEFAULT_PLUGIN_GROUP: &str = "org.apache.maven.plugins";

fn invalid(file: &str, message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: file.to_owned(),
        message: message.into(),
    }
}

fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .and_then(|c| c.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

fn parent_name<'a>(node: Node<'a, 'a>) -> Option<&'a str> {
    node.parent_element().map(|p| p.tag_name().name())
}

pub(crate) fn pom_xml(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let doc = Document::parse(text).map_err(|e| invalid("pom.xml", e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "project" {
        return Err(invalid("pom.xml", "root element is not <project>"));
    }

    let mut repositories = BTreeSet::new();
    let mut coordinates = BTreeSet::new();
    for node in root.descendants().filter(Node::is_element) {
        let line = doc.text_pos_at(node.range().start).row;
        let err = |m: &str| invalid("pom.xml", format!("line {line}: {m}"));
        let coordinate = |default_group: Option<&str>| -> Result<(String, String), LangError> {
            let group = child_text(node, "groupId")
                .or(default_group)
                .ok_or_else(|| err(&format!("<{}> without <groupId>", node.tag_name().name())))?;
            let artifact = child_text(node, "artifactId").ok_or_else(|| {
                err(&format!(
                    "<{}> without <artifactId>",
                    node.tag_name().name()
                ))
            })?;
            let version = child_text(node, "version").unwrap_or_default();
            Ok((format!("{group}:{artifact}"), version.to_owned()))
        };
        match (node.tag_name().name(), parent_name(node)) {
            ("repository", Some("repositories"))
            | ("pluginRepository", Some("pluginRepositories")) => {
                let url = child_text(node, "url").ok_or_else(|| err("repository without <url>"))?;
                repositories.insert(url.trim_end_matches('/').to_owned());
            }
            ("dependency", Some("dependencies")) => {
                coordinates.insert(coordinate(None)?);
            }
            ("plugin", Some("plugins")) => {
                coordinates.insert(coordinate(Some(DEFAULT_PLUGIN_GROUP))?);
            }
            ("extension", Some("extensions")) | ("parent", Some("project")) => {
                coordinates.insert(coordinate(None)?);
            }
            _ => {}
        }
    }

    let source = if repositories.is_empty() {
        MAVEN_CENTRAL.to_owned()
    } else {
        let extra: Vec<String> = repositories
            .into_iter()
            .map(|r| format!("repo={r}"))
            .collect();
        format!("{MAVEN_CENTRAL}+{}", extra.join("+"))
    };
    Ok(coordinates
        .into_iter()
        .map(|(name, version)| LockedDependency {
            name,
            version,
            source: source.clone(),
        })
        .collect())
}

/// `group:artifact:version=configurations` per line, as Gradle writes it.
/// Repositories live in the build script, which is not read here.
pub(crate) fn gradle_lockfile(file: &str, text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("empty=") {
            continue;
        }
        let err = || invalid(file, format!("line {}: cannot read `{line}`", i + 1));
        let (coordinate, _configurations) = line.split_once('=').ok_or_else(err)?;
        let parts: Vec<&str> = coordinate.split(':').collect();
        let [group, artifact, version] = parts.as_slice() else {
            return Err(err());
        };
        if [group, artifact, version].iter().any(|p| p.is_empty()) {
            return Err(err());
        }
        out.push(LockedDependency {
            name: format!("{group}:{artifact}"),
            version: (*version).to_owned(),
            source: MAVEN_CENTRAL.to_owned(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(deps: &[LockedDependency]) -> Vec<&str> {
        deps.iter().map(|d| d.name.as_str()).collect()
    }

    const POM: &str = r#"<?xml version="1.0"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent>
    <groupId>org.springframework.boot</groupId>
    <artifactId>spring-boot-starter-parent</artifactId>
    <version>3.4.1</version>
  </parent>
  <groupId>example</groupId>
  <artifactId>app</artifactId>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>com.example</groupId>
        <artifactId>bom</artifactId>
        <version>1.0</version>
        <type>pom</type>
        <scope>import</scope>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <dependencies>
    <dependency>
      <groupId>com.google.guava</groupId>
      <artifactId>guava</artifactId>
      <version>${guava.version}</version>
    </dependency>
  </dependencies>
  <build>
    <extensions>
      <extension>
        <groupId>kr.motd.maven</groupId>
        <artifactId>os-maven-plugin</artifactId>
        <version>1.7.1</version>
      </extension>
    </extensions>
    <plugins>
      <plugin>
        <artifactId>maven-surefire-plugin</artifactId>
        <version>3.5.2</version>
        <dependencies>
          <dependency>
            <groupId>org.ow2.asm</groupId>
            <artifactId>asm</artifactId>
            <version>9.7</version>
          </dependency>
        </dependencies>
      </plugin>
    </plugins>
  </build>
  <profiles>
    <profile>
      <id>extra</id>
      <dependencies>
        <dependency>
          <groupId>org.slf4j</groupId>
          <artifactId>slf4j-api</artifactId>
          <version>2.0.16</version>
        </dependency>
      </dependencies>
    </profile>
  </profiles>
  <distributionManagement>
    <repository><id>deploy</id><url>https://deploy.example/releases</url></repository>
  </distributionManagement>
</project>"#;

    #[test]
    fn reads_every_declared_coordinate() {
        let deps = pom_xml(POM).unwrap();
        assert_eq!(
            names(&deps),
            vec![
                "com.example:bom",
                "com.google.guava:guava",
                "kr.motd.maven:os-maven-plugin",
                "org.apache.maven.plugins:maven-surefire-plugin",
                "org.ow2.asm:asm",
                "org.slf4j:slf4j-api",
                "org.springframework.boot:spring-boot-starter-parent",
            ]
        );
        assert!(
            deps.iter().all(|d| d.source == MAVEN_CENTRAL),
            "deploy repo is not a source"
        );
        let guava = deps.iter().find(|d| d.name.ends_with("guava")).unwrap();
        assert_eq!(guava.version, "${guava.version}");
    }

    #[test]
    fn extra_repositories_change_every_source() {
        let pom = POM.replace(
            "<dependencies>\n    <dependency>\n      <groupId>com.google.guava",
            "<repositories><repository><id>x</id><url>https://evil.example/m2/</url></repository></repositories>\n  <dependencies>\n    <dependency>\n      <groupId>com.google.guava",
        );
        let deps = pom_xml(&pom).unwrap();
        assert!(
            deps.iter()
                .all(|d| d.source == format!("{MAVEN_CENTRAL}+repo=https://evil.example/m2"))
        );
        let plugin_repo = POM.replace(
            "<build>",
            "<pluginRepositories><pluginRepository><url>https://p.example</url></pluginRepository></pluginRepositories><build>",
        );
        assert!(
            pom_xml(&plugin_repo).unwrap()[0]
                .source
                .contains("repo=https://p.example")
        );
    }

    #[test]
    fn pom_fails_closed() {
        let err = |t: &str| pom_xml(t).unwrap_err().to_string();
        assert!(err("<notaproject/>").contains("not <project>"));
        assert!(err("<project>").contains("pom.xml"));
        assert!(err("<project><dependencies><dependency><artifactId>a</artifactId></dependency></dependencies></project>").contains("without <groupId>"));
        assert!(err("<project><repositories><repository><id>x</id></repository></repositories></project>").contains("without <url>"));
        assert!(
            err("<!DOCTYPE project [<!ENTITY x \"y\">]><project/>").contains("pom.xml"),
            "DTDs are refused"
        );
    }

    #[test]
    fn reads_gradle_lockfile() {
        let text = "# This is a Gradle generated file for dependency locking.
# Manual edits can break the build and are not advised.
com.google.guava:guava:33.0.0-jre=compileClasspath,runtimeClasspath
org.junit.jupiter:junit-jupiter:5.11.4=testCompileClasspath
empty=annotationProcessor
";
        let deps = gradle_lockfile("gradle.lockfile", text).unwrap();
        assert_eq!(
            names(&deps),
            vec!["com.google.guava:guava", "org.junit.jupiter:junit-jupiter"]
        );
        assert_eq!(deps[0].version, "33.0.0-jre");
        assert!(gradle_lockfile("gradle.lockfile", "guava=compile\n").is_err());
        assert!(gradle_lockfile("gradle.lockfile", "a:b:c\n").is_err());
    }
}
