//! Model per tier, the `[models]` section of `config/*.toml` (design §5.1).

use aegis_ledger::Tier;
use aegis_provider::Effort;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierModel {
    pub id: String,
    #[serde(default)]
    pub effort: Option<Effort>,
}

/// Deserialized by the caller from `[models]`; the router only takes the value.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    pub light: TierModel,
    pub core: TierModel,
    pub expert: TierModel,
}

impl ModelsConfig {
    pub fn for_tier(&self, tier: Tier) -> &TierModel {
        match tier {
            Tier::Light => &self.light,
            Tier::Core => &self.core,
            Tier::Expert => &self.expert,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct RunConfig {
        models: ModelsConfig,
    }

    /// Mirrors how `aegis-cli` reads the run config: other sections ignored.
    fn models(config: &str) -> ModelsConfig {
        toml::from_str::<RunConfig>(config).unwrap().models
    }

    #[test]
    fn reads_poc_config() {
        let m = models(include_str!("../../../config/poc.toml"));
        assert_eq!(m.for_tier(Tier::Light).id, "claude-haiku-5-5");
        assert_eq!(m.for_tier(Tier::Core).id, "claude-sonnet-5-5");
        assert_eq!(m.for_tier(Tier::Core).effort, None);
        assert_eq!(
            m.for_tier(Tier::Expert),
            &TierModel {
                id: "claude-opus-5-5".into(),
                effort: Some(Effort::High),
            }
        );
    }

    #[test]
    fn reads_customer_config() {
        let m = models(include_str!("../../../config/customer-jp.toml"));
        assert_eq!(m.core.id, "jp.anthropic.claude-opus-5-5");
        assert_eq!(m.core.effort, Some(Effort::Low));
        assert_eq!(m.expert.effort, Some(Effort::High));
        assert!(
            [&m.light, &m.core, &m.expert]
                .iter()
                .all(|t| t.id.starts_with("jp."))
        );
    }

    #[test]
    fn rejects_missing_tier_and_unknown_keys() {
        let missing = "[models.light]\nid = \"a\"\n[models.core]\nid = \"b\"\n";
        assert!(toml::from_str::<RunConfig>(missing).is_err());
        let typo = "[models.light]\nid = \"a\"\nefort = \"low\"\n\
                    [models.core]\nid = \"b\"\n[models.expert]\nid = \"c\"\n";
        assert!(toml::from_str::<RunConfig>(typo).is_err());
    }
}
