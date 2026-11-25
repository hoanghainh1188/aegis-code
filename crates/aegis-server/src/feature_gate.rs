//! The feature lane's design gate as the server reads it (#677): the
//! repo's cap on revisions, and the delivery modes a person may choose on
//! approval.

use aegis_lifecycle::FeatureLane;
use aegis_store::DeliveryMode;

/// How many revisions of a plan the repo allows (`[feature_lane]
/// max_revisions`), from its settings; the lane's default when unset or
/// unreadable.
pub fn max_revisions(config: &serde_json::Value) -> u32 {
    #[derive(serde::Deserialize)]
    struct Partial {
        #[serde(default)]
        feature_lane: FeatureLane,
    }
    serde_json::from_value::<Partial>(config.clone())
        .map(|p| p.feature_lane.max_revisions)
        .unwrap_or_else(|_| FeatureLane::default().max_revisions)
}

/// A delivery mode as a person writes it, `integration_pr` or
/// `stacked_prs`; why not, otherwise.
pub fn parse_mode(mode: &str) -> Result<DeliveryMode, String> {
    DeliveryMode::parse(mode)
        .ok_or_else(|| format!("mode `{mode}` is not `integration_pr` or `stacked_prs`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_comes_from_the_repo_s_settings() {
        let cap = |config: serde_json::Value| max_revisions(&config);
        assert_eq!(cap(serde_json::json!({})), 3);
        assert_eq!(
            cap(serde_json::json!({ "feature_lane": { "enabled": true, "max_revisions": 1 } })),
            1
        );
        assert_eq!(
            cap(serde_json::json!({ "feature_lane": { "bogus": 1 } })),
            3
        );
        assert_eq!(parse_mode("stacked_prs"), Ok(DeliveryMode::StackedPrs));
        assert!(parse_mode("auto").is_err());
    }
}
