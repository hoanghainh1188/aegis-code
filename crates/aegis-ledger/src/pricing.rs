//! Model prices from `config/pricing.toml` and the input-cost formula
//! (design §4.3):
//!
//! `C_in = R · (N_read·M_read + N_write·M_write + N_uncached)`, plus output,
//! times `1 + regional_surcharge` on the Bedrock configuration (§5.4). A
//! model priced by prompt length (Haiku 5.5) takes its `long_prompt` R and
//! output price for a call whose prompt is over the threshold (#54).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::LedgerError;
use crate::record::TokenCounts;

const PER_MILLION: f64 = 1_000_000.0;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceTable {
    regional_surcharge: f64,
    cache_write_multiplier: WriteMultiplier,
    models: BTreeMap<String, ModelPrice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteMultiplier {
    #[serde(rename = "5m")]
    five_minutes: f64,
    #[serde(rename = "1h")]
    one_hour: f64,
}

/// Prices in $ per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_read_multiplier: f64,
    #[serde(default)]
    pub long_prompt: Option<LongPrompt>,
}

/// Prices of a call whose prompt (input, cache reads and cache writes
/// together) is over `over_tokens`. The whole call pays them, output too;
/// the cache multipliers stay the same.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LongPrompt {
    pub over_tokens: u64,
    pub input: f64,
    pub output: f64,
}

impl PriceTable {
    pub fn from_toml(text: &str) -> Result<Self, LedgerError> {
        let table: Self =
            toml::from_str(text).map_err(|e| LedgerError::InvalidPrices(e.to_string()))?;
        table.validate()?;
        Ok(table)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(LedgerError::io(path))?;
        Self::from_toml(&text)
    }

    /// Price for a model ID. Bedrock IDs such as `jp.anthropic.claude-opus-5-5`
    /// are looked up by the part after `anthropic.`.
    pub fn price(&self, model: &str) -> Result<&ModelPrice, LedgerError> {
        let base = model.rsplit_once("anthropic.").map_or(model, |(_, id)| id);
        self.models
            .get(model)
            .or_else(|| self.models.get(base))
            .ok_or_else(|| LedgerError::UnknownModel(model.to_owned()))
    }

    /// Cost in $ of one model call. `regional` adds the Bedrock regional
    /// surcharge.
    pub fn cost(
        &self,
        model: &str,
        tokens: &TokenCounts,
        regional: bool,
    ) -> Result<f64, LedgerError> {
        let price = self.price(model)?;
        let prompt =
            tokens.input + tokens.cache_read + tokens.cache_write_5m + tokens.cache_write_1h;
        let (input_price, output_price) = match price.long_prompt {
            Some(long) if prompt > long.over_tokens => (long.input, long.output),
            _ => (price.input, price.output),
        };
        let weighted_input = tokens.cache_read as f64 * price.cache_read_multiplier
            + tokens.cache_write_5m as f64 * self.cache_write_multiplier.five_minutes
            + tokens.cache_write_1h as f64 * self.cache_write_multiplier.one_hour
            + tokens.input as f64;
        let base =
            (input_price * weighted_input + output_price * tokens.output as f64) / PER_MILLION;
        let surcharge = if regional {
            1.0 + self.regional_surcharge
        } else {
            1.0
        };
        Ok(base * surcharge)
    }

    fn validate(&self) -> Result<(), LedgerError> {
        let invalid = |what: String| Err(LedgerError::InvalidPrices(what));
        if !(0.0..1.0).contains(&self.regional_surcharge) {
            return invalid(format!(
                "regional_surcharge must be in [0, 1), got {}",
                self.regional_surcharge
            ));
        }
        let WriteMultiplier {
            five_minutes,
            one_hour,
        } = self.cache_write_multiplier;
        if !non_negative(five_minutes) || !non_negative(one_hour) {
            return invalid("cache_write_multiplier must be finite and >= 0".into());
        }
        if self.models.is_empty() {
            return invalid("no models".into());
        }
        for (id, p) in &self.models {
            if ![p.input, p.output, p.cache_read_multiplier]
                .into_iter()
                .all(non_negative)
            {
                return invalid(format!("prices for `{id}` must be finite and >= 0"));
            }
            let bad_long = |l: &LongPrompt| {
                l.over_tokens == 0 || !non_negative(l.input) || !non_negative(l.output)
            };
            if p.long_prompt.as_ref().is_some_and(bad_long) {
                return invalid(format!(
                    "long_prompt of `{id}` needs over_tokens > 0 and finite prices >= 0"
                ));
            }
        }
        Ok(())
    }
}

fn non_negative(x: f64) -> bool {
    x.is_finite() && x >= 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = include_str!("../../../config/pricing.toml");

    fn table() -> PriceTable {
        PriceTable::from_toml(SHIPPED).unwrap()
    }

    fn tokens(input: u64, output: u64, write_5m: u64, write_1h: u64, read: u64) -> TokenCounts {
        TokenCounts {
            input,
            output,
            cache_write_5m: write_5m,
            cache_write_1h: write_1h,
            cache_read: read,
        }
    }

    fn assert_usd(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected ${expected}, got ${actual}"
        );
    }

    #[test]
    fn shipped_table_matches_proposal_prices() {
        let t = table();
        let sonnet = t.price("claude-sonnet-5-5").unwrap();
        assert_eq!(
            (sonnet.input, sonnet.output, sonnet.cache_read_multiplier),
            (2.0, 10.0, 0.05)
        );
        let haiku = t.price("claude-haiku-5-5").unwrap();
        assert_eq!(haiku.cache_read_multiplier, 0.10);
        let long = haiku.long_prompt.unwrap();
        assert_eq!(
            (long.over_tokens, long.input, long.output),
            (100_000, 0.50, 2.50)
        );
        assert_eq!(t.price("claude-fable-5-1").unwrap().output, 50.0);
        assert_eq!(t.price("claude-opus-5-5").unwrap().input, 4.0);
    }

    /// A model a shipped config sends but the table does not price fails
    /// the run at the first call (#221).
    #[test]
    fn every_model_of_the_shipped_configs_has_a_price() {
        let t = table();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let config: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
            let Some(models) = config.get("models").and_then(|m| m.as_table()) else {
                continue;
            };
            for tier in models.values() {
                if let Some(id) = tier.get("id").and_then(|id| id.as_str()) {
                    assert!(t.price(id).is_ok(), "{}: no price for {id}", path.display());
                    checked += 1;
                }
            }
        }
        assert!(checked > 0);
    }

    // Sonnet, 5-minute writes:
    // C_in  = 2 · (50 000·0.05 + 2 000·1.25 + 1 000) / 1e6 = 2 · 6 000 / 1e6 = 0.012
    // C_out = 800 · 10 / 1e6 = 0.008
    #[test]
    fn sonnet_call_with_cache_read_and_5m_write() {
        let cost = table()
            .cost(
                "claude-sonnet-5-5",
                &tokens(1_000, 800, 2_000, 0, 50_000),
                false,
            )
            .unwrap();
        assert_usd(cost, 0.020);
    }

    // Opus via Bedrock `jp.` profile, 1-hour writes, +10%:
    // C_in  = 4 · (10 000·2 + 4 000) / 1e6 = 0.096
    // C_out = 2 000 · 20 / 1e6 = 0.04
    // total = 0.136 · 1.1 = 0.1496
    #[test]
    fn bedrock_profile_uses_base_price_and_surcharge() {
        let cost = table()
            .cost(
                "jp.anthropic.claude-opus-5-5",
                &tokens(4_000, 2_000, 0, 10_000, 0),
                true,
            )
            .unwrap();
        assert_usd(cost, 0.1496);
    }

    // Haiku reads at 10%:
    // C_in  = 0.1 · (20 000·0.1 + 3 000) / 1e6 = 0.0005
    // C_out = 500 · 0.5 / 1e6 = 0.00025
    #[test]
    fn haiku_reads_at_ten_percent() {
        let cost = table()
            .cost("claude-haiku-5-5", &tokens(3_000, 500, 0, 0, 20_000), false)
            .unwrap();
        assert_usd(cost, 0.00075);
    }

    // Fable, both write TTLs in one call:
    // C_in = 10 · (10 000·0.025 + 1 000·1.25 + 1 000·2) / 1e6 = 10 · 3 500 / 1e6 = 0.035
    #[test]
    fn fable_prices_each_write_ttl_separately() {
        let cost = table()
            .cost(
                "claude-fable-5-1",
                &tokens(0, 0, 1_000, 1_000, 10_000),
                false,
            )
            .unwrap();
        assert_usd(cost, 0.035);
    }

    #[test]
    fn surcharge_only_applies_when_regional() {
        let t = table();
        let usage = tokens(1_000_000, 0, 0, 0, 0);
        assert_usd(t.cost("claude-sonnet-5-5", &usage, false).unwrap(), 2.0);
        assert_usd(t.cost("claude-sonnet-5-5", &usage, true).unwrap(), 2.2);
    }

    #[test]
    fn unknown_model_is_an_error_not_free() {
        let err = table()
            .cost("claude-unknown-9", &TokenCounts::default(), false)
            .unwrap_err();
        assert!(matches!(err, LedgerError::UnknownModel(m) if m == "claude-unknown-9"));
    }

    // Haiku, prompt of exactly 100 000 tokens (60 000 input + 40 000 read):
    // not over the threshold, base prices.
    // C_in  = 0.10 · (40 000·0.10 + 60 000) / 1e6 = 0.10 · 64 000 / 1e6 = 0.0064
    // C_out = 1 000 · 0.50 / 1e6 = 0.0005
    #[test]
    fn haiku_at_the_threshold_pays_base_prices() {
        let cost = table()
            .cost(
                "claude-haiku-5-5",
                &tokens(60_000, 1_000, 0, 0, 40_000),
                false,
            )
            .unwrap();
        assert_usd(cost, 0.0069);
    }

    // One token more (60 001 + 40 000 = 100 001): the whole call, output too,
    // at the long-prompt prices; the read multiplier stays 0.10.
    // C_in  = 0.50 · (40 000·0.10 + 60 001) / 1e6 = 0.50 · 64 001 / 1e6 = 0.0320005
    // C_out = 1 000 · 2.50 / 1e6 = 0.0025
    #[test]
    fn haiku_over_the_threshold_pays_long_prompt_prices_for_the_whole_call() {
        let cost = table()
            .cost(
                "claude-haiku-5-5",
                &tokens(60_001, 1_000, 0, 0, 40_000),
                false,
            )
            .unwrap();
        assert_usd(cost, 0.0345005);
    }

    // Cache writes count toward the prompt (1 + 100 000 written for 1 h):
    // C_in = 0.50 · (100 000·2.0 + 1) / 1e6 = 0.1000005
    // With the regional surcharge: · 1.10 = 0.11000055
    #[test]
    fn cache_writes_count_toward_the_prompt_length() {
        let t = table();
        let long = tokens(1, 0, 0, 100_000, 0);
        assert_usd(t.cost("claude-haiku-5-5", &long, false).unwrap(), 0.1000005);
        assert_usd(t.cost("claude-haiku-5-5", &long, true).unwrap(), 0.11000055);
        // Models without a long-prompt price keep theirs at any length.
        let sonnet = t.cost("claude-sonnet-5-5", &tokens(200_000, 0, 0, 0, 0), false);
        assert_usd(sonnet.unwrap(), 0.4);
    }

    #[test]
    fn rejects_bad_long_prompt_prices() {
        for (from, to) in [
            ("over_tokens = 100000", "over_tokens = 0"),
            ("output = 2.50", "output = -2.50"),
            ("over_tokens = 100000", "over = 100000"),
        ] {
            let text = SHIPPED.replace(from, to);
            assert!(PriceTable::from_toml(&text).is_err(), "{to}");
        }
    }

    #[test]
    fn rejects_bad_tables() {
        let cases = [
            ("regional_surcharge = 1.5", "regional_surcharge"),
            ("regional_surcharge = -0.1", "regional_surcharge"),
        ];
        for (line, needle) in cases {
            let text = SHIPPED.replace("regional_surcharge = 0.10", line);
            let err = PriceTable::from_toml(&text).unwrap_err().to_string();
            assert!(err.contains(needle), "{line}: {err}");
        }
        let negative = SHIPPED.replace("output = 10.0", "output = -10.0");
        assert!(PriceTable::from_toml(&negative).is_err());
        let typo = SHIPPED.replace("cache_read_multiplier = 0.025", "cache_read = 0.025");
        assert!(PriceTable::from_toml(&typo).is_err());
        let no_models =
            "regional_surcharge = 0.1\n[cache_write_multiplier]\n5m = 1.25\n1h = 2.0\n[models]\n";
        assert!(PriceTable::from_toml(no_models).is_err());
    }

    #[test]
    fn every_configured_model_has_a_price() {
        let t = table();
        let configs = [
            include_str!("../../../config/poc.toml"),
            include_str!("../../../config/customer-jp.toml"),
        ];
        for config in configs {
            let value: toml::Table = toml::from_str(config).unwrap();
            for (tier, model) in value["models"].as_table().unwrap() {
                let id = model["id"].as_str().unwrap();
                assert!(t.price(id).is_ok(), "{tier}: `{id}` has no price");
            }
        }
    }

    #[test]
    fn load_reports_missing_file() {
        let err = PriceTable::load("/nonexistent/pricing.toml").unwrap_err();
        assert!(matches!(err, LedgerError::Io { .. }));
    }
}
