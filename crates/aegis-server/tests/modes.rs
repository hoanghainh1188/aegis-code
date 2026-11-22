//! #592: `[modes]` takes any mode by name, each with its own residency,
//! and a worker does not start with a mode that would send code where its
//! `[residency]` says it may not go. No database needed.

use std::path::{Path, PathBuf};

use aegis_provider::{ModelPolicy, ProviderConfig};
use aegis_server::config::ServerConfig;
use aegis_store::ModeName;

const POC: &str = include_str!("../../../config/poc.toml");
const JP: &str = include_str!("../../../config/customer-jp.toml");
const LOCAL: &str = include_str!("../../../config/local.toml");

/// A directory of its own under the system's temp dir, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("aegis-modes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, file: &str, text: &str) {
        std::fs::write(self.0.join(file), text).unwrap();
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `customer-jp.toml` moved to Frankfurt, its models `prefix`.
fn eu(prefix: &str) -> String {
    JP.replace(
        "allowed_model_prefixes = [\"jp.\"]",
        "allowed_model_prefixes = [\"eu.\"]",
    )
    .replace("ap-northeast-1", "eu-central-1")
    .replace("require_jp_profile = true", "require_jp_profile = false")
    .replace("\"jp.", &format!("\"{prefix}"))
}

fn airgap() -> String {
    format!("{LOCAL}\n[residency]\nlocal_only = true\n")
}

fn server(modes: &str) -> ServerConfig {
    ServerConfig::from_toml(&format!("[modes]\n{modes}")).unwrap()
}

#[test]
fn custom_modes_load_with_their_residency_and_the_old_spelling_still_works() {
    let dir = Dir::new("custom");
    dir.write("poc.toml", POC);
    dir.write("customer-jp.toml", JP);
    dir.write("customer-eu.toml", &eu("eu."));
    dir.write("airgap.toml", &airgap());
    let config = server(
        "poc = \"poc.toml\"\n\
         customer_jp = \"customer-jp.toml\"\n\
         customer-eu = \"customer-eu.toml\"\n\
         local-airgap = \"airgap.toml\"\n",
    );
    let modes = config.load_modes(dir.path()).unwrap();

    let names: Vec<&str> = modes.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["customer-eu", "customer_jp", "local-airgap", "poc"]);
    let policy = |name: &str| {
        let name = ModeName::parse(name).unwrap();
        modes.config(&name).unwrap().check(&name).unwrap()
    };
    assert_eq!(
        policy("customer-eu"),
        ModelPolicy::AllowedPrefixes(vec!["eu.".into()])
    );
    assert_eq!(policy("local-airgap"), ModelPolicy::LocalOnly);
    assert_eq!(policy("customer-jp"), ModelPolicy::japan_only());
    assert_eq!(policy("poc"), ModelPolicy::Any);
    let jp = modes.config(&ModeName::CUSTOMER_JP).unwrap();
    assert!(matches!(
        jp.provider,
        ProviderConfig::Bedrock {
            require_jp_profile: true,
            ..
        }
    ));
}

#[test]
fn a_mode_with_a_model_outside_its_prefixes_stops_the_start() {
    let dir = Dir::new("outside");
    dir.write("customer-eu.toml", &eu("global."));
    let err = server("customer-eu = \"customer-eu.toml\"\n")
        .load_modes(dir.path())
        .unwrap_err();
    assert!(err.contains("customer-eu"), "{err}");
    assert!(err.contains("global.anthropic.claude-haiku-5-5"), "{err}");
    assert!(err.contains("`eu.`"), "{err}");
}

#[test]
fn a_local_only_mode_on_a_cloud_provider_stops_the_start() {
    let dir = Dir::new("cloud");
    dir.write(
        "airgap.toml",
        &format!("{POC}\n[residency]\nlocal_only = true\n"),
    );
    let err = server("local-airgap = \"airgap.toml\"\n")
        .load_modes(dir.path())
        .unwrap_err();
    assert!(err.contains("local_only"), "{err}");
}

#[test]
fn a_customer_jp_mode_that_leaves_japan_stops_the_start_under_either_spelling() {
    let dir = Dir::new("jp");
    dir.write(
        "customer-jp.toml",
        &JP.replace(
            "id = \"jp.anthropic.claude-haiku-5-5\"",
            "id = \"global.anthropic.claude-haiku-5-5\"",
        ),
    );
    for key in ["customer_jp", "customer-jp"] {
        let err = server(&format!("{key} = \"customer-jp.toml\"\n"))
            .load_modes(dir.path())
            .unwrap_err();
        assert!(err.contains("customer-jp"), "{key}: {err}");
    }
}

#[test]
fn a_worker_needs_a_mode() {
    let err = ServerConfig::from_toml("")
        .unwrap()
        .load_modes(Path::new("."))
        .unwrap_err();
    assert!(err.contains("[modes]"), "{err}");
}
