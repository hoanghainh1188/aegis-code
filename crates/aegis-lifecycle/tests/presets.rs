//! #599: each Self-Hosted preset in `deploy/presets/` loads as a mode,
//! keeps to its own residency (the customer-jp guard for the Japan one,
//! the embedding endpoint included), and prices every model it sends.

use std::path::{Path, PathBuf};

use aegis_ledger::PriceTable;
use aegis_lifecycle::ModeConfig;
use aegis_provider::{ModelPolicy, ProviderConfig, ResidencyConfig};
use aegis_store::ModeName;

const PRESETS: [&str; 4] = [
    "aws-japan-bedrock",
    "gcp-vertex-tokyo",
    "azure-openai-enterprise",
    "onprem-airgap-ollama",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn preset(name: &str) -> ModeConfig {
    let path = repo().join("deploy/presets").join(name).join("mode.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
    ModeConfig::from_toml(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The pricing file a mode names, as the server reads it: from the image
/// (`config/…`, under /srv/aegis) or from the preset's own directory
/// (`/etc/aegis/modes/…`, where the preset's README copies it).
fn pricing(name: &str, mode: &ModeConfig) -> PriceTable {
    let path = &mode.run.pricing;
    let file = match path.strip_prefix("/etc/aegis/modes") {
        Ok(rest) => repo().join("deploy/presets").join(name).join(rest),
        Err(_) => repo().join(path),
    };
    PriceTable::load(&file).unwrap_or_else(|e| panic!("{name}: {}: {e}", file.display()))
}

#[test]
fn every_preset_loads_keeps_to_its_residency_and_prices_its_models() {
    let found: Vec<String> = std::fs::read_dir(repo().join("deploy/presets"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for name in &found {
        assert!(PRESETS.contains(&name.as_str()), "{name}: not tested here");
    }
    for name in PRESETS {
        let mode = preset(name);
        let key = if name == "aws-japan-bedrock" {
            ModeName::CUSTOMER_JP
        } else {
            ModeName::parse(name).unwrap()
        };
        mode.check(&key).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_ne!(
            mode.residency,
            ResidencyConfig::default(),
            "{name}: a preset says where code may go"
        );
        assert!(
            mode.memory.embedding.is_some(),
            "{name}: semantic recall configured"
        );
        let prices = pricing(name, &mode);
        for model in [&mode.models.light, &mode.models.core, &mode.models.expert] {
            assert!(
                prices.price(&model.id).is_ok(),
                "{name}: no price for {}",
                model.id
            );
        }
    }
}

#[test]
fn the_airgap_preset_sends_nothing_off_the_network() {
    let mode = preset("onprem-airgap-ollama");
    assert_eq!(
        mode.check(&ModeName::parse("onprem-airgap-ollama").unwrap())
            .unwrap(),
        ModelPolicy::LocalOnly
    );
    assert!(matches!(mode.provider, ProviderConfig::Local { .. }));
    assert_eq!(mode.budget.max_cost_usd, 0.0);
}
