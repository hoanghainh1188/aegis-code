//! Java dependency readers (#68): `pom.xml` and `gradle.lockfile`.
//!
//! Maven has no lockfile, so `pom.xml` gives what the repo declares: every
//! `dependency`, `plugin` and build `extension` anywhere in the file
//! (dependency management, Maven profiles, plugin dependencies) and the
//! parent, since plugins and extensions are code that runs at build time.
//! Extra `<repositories>` change every coordinate's source, like extra
//! indexes for Python. Transitive dependencies need a resolved tree from
//! Maven itself (`maven_tree`, #80). Fails closed on what it cannot read.

use std::collections::{BTreeMap, BTreeSet};

use roxmltree::{Document, Node};

use crate::{LangError, LockedDependency};

/// Maven Central, the official repository.
pub(crate) const MAVEN_CENTRAL: &str = "https://repo.maven.apache.org/maven2";
const DEFAULT_PLUGIN_GROUP: &str = "org.apache.maven.plugins";
/// Property references resolved inside one another, at most.
const MAX_PROPERTY_DEPTH: usize = 8;

fn invalid(file: &str, message: impl Into<String>) -> LangError {
    LangError::InvalidLockfile {
        file: file.to_owned(),
        message: message.into(),
    }
}

fn child<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
}

/// Text of a child element as Maven reads it: every text node joined.
/// A comment, processing instruction or element inside the value could
/// make it read differently than it looks (`com.<!---->evil`), so it is an
/// error.
fn child_text(node: Node<'_, '_>, name: &str) -> Result<Option<String>, String> {
    let Some(el) = child(node, name) else {
        return Ok(None);
    };
    if el.children().any(|c| !c.is_text()) {
        return Err(format!("<{name}> must hold plain text only"));
    }
    let text: String = el.children().filter_map(|c| c.text()).collect();
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

fn parent_name<'a>(node: Node<'a, 'a>) -> Option<&'a str> {
    node.parent_element().map(|p| p.tag_name().name())
}

/// `${…}` references the pom can resolve by itself: `project.*`,
/// `project.parent.*` and top-level `<properties>`. Properties set in Maven
/// profiles, `.mvn/maven.config` or the command line are not known here.
struct Properties(BTreeMap<String, String>);

impl Properties {
    fn of(root: Node<'_, '_>) -> Result<Self, String> {
        let mut map = BTreeMap::new();
        if let Some(props) = child(root, "properties") {
            for p in props.children().filter(Node::is_element) {
                let name = p.tag_name().name().to_owned();
                if let Some(value) = child_text(props, &name)? {
                    map.insert(name, value);
                }
            }
        }
        let parent = child(root, "parent");
        for (key, owner, field) in [
            ("project.parent.groupId", parent, "groupId"),
            ("project.parent.artifactId", parent, "artifactId"),
            ("project.parent.version", parent, "version"),
            ("project.groupId", Some(root), "groupId"),
            ("project.artifactId", Some(root), "artifactId"),
            ("project.version", Some(root), "version"),
        ] {
            if let Some(value) = owner.map(|o| child_text(o, field)).transpose()?.flatten() {
                map.insert(key.to_owned(), value);
            }
        }
        // Inherited from the parent when the project does not set them.
        for field in ["groupId", "version"] {
            let own = format!("project.{field}");
            let inherited = format!("project.parent.{field}");
            if !map.contains_key(&own)
                && let Some(v) = map.get(&inherited).cloned()
            {
                map.insert(own, v);
            }
        }
        Ok(Self(map))
    }

    fn resolve(&self, value: &str) -> Result<String, String> {
        let mut out = value.to_owned();
        for _ in 0..MAX_PROPERTY_DEPTH {
            let Some(start) = out.find("${") else {
                return Ok(out);
            };
            let end = out[start..]
                .find('}')
                .map(|e| start + e)
                .ok_or_else(|| format!("unclosed `${{` in `{value}`"))?;
            let key = &out[start + 2..end];
            let replacement = self
                .0
                .get(key)
                .ok_or_else(|| format!("`${{{key}}}` in `{value}` is not set in this pom"))?;
            out = format!("{}{replacement}{}", &out[..start], &out[end + 1..]);
        }
        Err(format!("`{value}`: properties nested too deep"))
    }
}

pub(crate) fn pom_xml(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let doc = Document::parse(text).map_err(|e| invalid("pom.xml", e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "project" {
        return Err(invalid("pom.xml", "root element is not <project>"));
    }
    let properties = Properties::of(root).map_err(|m| invalid("pom.xml", m))?;

    let mut repositories = BTreeSet::new();
    let mut coordinates = BTreeSet::new();
    for node in root.descendants().filter(Node::is_element) {
        let line = doc.text_pos_at(node.range().start).row;
        let err = |m: String| invalid("pom.xml", format!("line {line}: {m}"));
        let tag = node.tag_name().name();
        let field = |name: &str| -> Result<Option<String>, LangError> {
            child_text(node, name)
                .and_then(|v| v.map(|v| properties.resolve(&v)).transpose())
                .map_err(err)
        };
        let coordinate = |default_group: Option<&str>| -> Result<(String, String), LangError> {
            let group = field("groupId")?
                .or(default_group.map(str::to_owned))
                .ok_or_else(|| err(format!("<{tag}> without <groupId>")))?;
            let artifact =
                field("artifactId")?.ok_or_else(|| err(format!("<{tag}> without <artifactId>")))?;
            let version = child_text(node, "version")
                .map_err(err)?
                .unwrap_or_default();
            Ok((format!("{group}:{artifact}"), version))
        };
        match (tag, parent_name(node)) {
            ("repository", Some("repositories"))
            | ("pluginRepository", Some("pluginRepositories")) => {
                let url = field("url")?.ok_or_else(|| err("repository without <url>".into()))?;
                repositories.insert(url.trim_end_matches('/').to_owned());
            }
            ("dependency", Some("dependencies")) => {
                // A jar from a path on disk, outside any repository.
                let scope = child_text(node, "scope").map_err(err)?;
                if child(node, "systemPath").is_some() || scope.as_deref() == Some("system") {
                    return Err(err("system-scoped dependencies are not allowed".into()));
                }
                coordinates.insert(coordinate(None)?);
            }
            ("plugin", Some("plugins")) => {
                coordinates.insert(coordinate(Some(DEFAULT_PLUGIN_GROUP))?);
            }
            ("extension", Some("extensions")) | ("parent", Some("project")) => {
                coordinates.insert(coordinate(None)?);
            }
            ("module", Some("modules")) => {
                // Maven also accepts a pom file of any name, which no
                // `pom.xml` pattern would check.
                let module = node.text().unwrap_or_default().trim();
                if module.is_empty() || module.ends_with(".xml") || module.contains("..") {
                    return Err(err(format!(
                        "module `{module}` must be a directory in the repo"
                    )));
                }
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

/// `.mvn/extensions.xml`: core extensions, code Maven loads before the
/// build.
pub(crate) fn maven_extensions(text: &str) -> Result<Vec<LockedDependency>, LangError> {
    let file = "extensions.xml";
    let doc = Document::parse(text).map_err(|e| invalid(file, e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "extensions" {
        return Err(invalid(file, "root element is not <extensions>"));
    }
    root.children()
        .filter(|c| c.is_element() && c.tag_name().name() == "extension")
        .map(|ext| {
            let text = |name: &str| {
                child_text(ext, name)
                    .map_err(|m| invalid(file, m))?
                    .filter(|v| !v.contains("${"))
                    .ok_or_else(|| invalid(file, format!("<extension> needs a literal <{name}>")))
            };
            Ok(LockedDependency {
                name: format!("{}:{}", text("groupId")?, text("artifactId")?),
                version: text("version")?,
                source: MAVEN_CENTRAL.to_owned(),
            })
        })
        .collect()
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

    fn pom(body: &str) -> String {
        format!(
            "<project><groupId>example</groupId><artifactId>app</artifactId><version>1.0</version>{body}</project>"
        )
    }

    fn dependency(group: &str, artifact: &str) -> String {
        format!(
            "<dependencies><dependency><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>1</version></dependency></dependencies>"
        )
    }

    #[test]
    fn comments_inside_values_are_refused() {
        let err = |b: &str| pom_xml(&pom(b)).unwrap_err().to_string();
        assert!(err(&dependency("com.google.guava<!---->.evil", "guava")).contains("plain text"));
        assert!(err(&dependency("com.google.guava", "<!--x-->guava")).contains("plain text"));
        let plugin = "<build><plugins><plugin><groupId><!--x-->org.evil</groupId><artifactId>p</artifactId></plugin></plugins></build>";
        assert!(err(plugin).contains("plain text"));
        let split = "<dependencies><dependency><groupId>com.<?x?>evil</groupId><artifactId>a</artifactId></dependency></dependencies>";
        assert!(err(split).contains("plain text"));
    }

    #[test]
    fn properties_are_resolved_or_refused() {
        let body = format!(
            "<properties><lib.group>com.example.libs</lib.group></properties>{}",
            dependency("${lib.group}", "core-${project.artifactId}")
        );
        assert_eq!(
            names(&pom_xml(&pom(&body)).unwrap()),
            vec!["com.example.libs:core-app"]
        );
        // Changing only the property changes the coordinate.
        let evil = body.replace("com.example.libs", "com.evil");
        assert_eq!(
            names(&pom_xml(&pom(&evil)).unwrap()),
            vec!["com.evil:core-app"]
        );
        // Set elsewhere (profile, maven.config, parent): unknown here.
        let unknown = pom_xml(&pom(&dependency("${outside.group}", "lib"))).unwrap_err();
        assert!(unknown.to_string().contains("not set in this pom"));
        let repo = "<repositories><repository><url>${repo.url}</url></repository></repositories>";
        assert!(pom_xml(&pom(repo)).is_err());
    }

    #[test]
    fn group_and_version_inherit_from_the_parent() {
        let text = "<project><parent><groupId>org.acme</groupId><artifactId>parent</artifactId><version>2.0</version></parent>\
            <artifactId>app</artifactId><dependencies><dependency><groupId>${project.groupId}</groupId>\
            <artifactId>sibling</artifactId><version>${project.version}</version></dependency></dependencies></project>";
        let deps = pom_xml(text).unwrap();
        assert_eq!(names(&deps), vec!["org.acme:parent", "org.acme:sibling"]);
    }

    #[test]
    fn system_scope_and_odd_modules_are_refused() {
        let err = |b: &str| pom_xml(&pom(b)).unwrap_err().to_string();
        let system = "<dependencies><dependency><groupId>a</groupId><artifactId>b</artifactId><scope>system</scope><systemPath>${basedir}/lib/evil.jar</systemPath></dependency></dependencies>";
        assert!(err(system).contains("system-scoped"));
        assert!(
            err("<modules><module>evil.xml</module></modules>").contains("must be a directory")
        );
        assert!(
            err("<modules><module>../outside</module></modules>").contains("must be a directory")
        );
        assert!(pom_xml(&pom("<modules><module>core</module></modules>")).is_ok());
    }

    #[test]
    fn reads_maven_core_extensions() {
        let text = "<extensions><extension><groupId>kr.motd.maven</groupId><artifactId>os-maven-plugin</artifactId><version>1.7.1</version></extension></extensions>";
        assert_eq!(
            names(&maven_extensions(text).unwrap()),
            vec!["kr.motd.maven:os-maven-plugin"]
        );
        assert!(maven_extensions("<extensions><extension><groupId>${g}</groupId><artifactId>a</artifactId><version>1</version></extension></extensions>").is_err());
        assert!(maven_extensions("<project/>").is_err());
    }
}
