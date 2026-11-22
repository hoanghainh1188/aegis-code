//! Data-residency guard (design §7, #592): which models a configuration
//! may send code to, and through which endpoint. `customer-jp` keeps every
//! request on a `jp.` Bedrock inference profile; other modes declare their
//! own rule in a `[residency]` section of their run configuration.

use std::net::IpAddr;

use serde::Deserialize;
use url::{Host, Url};

use crate::{ProviderConfig, ProviderError};

const JAPAN_PROFILE_PREFIX: &str = "jp.";

/// What a provider checks before every request it sends.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelPolicy {
    #[default]
    Any,
    /// Only model IDs starting with one of these prefixes (`jp.`, `eu.`,
    /// `us.`…) are sent. Never empty.
    AllowedPrefixes(Vec<String>),
    /// Only a `local` provider (Ollama, vLLM… on the deployment's own
    /// network) is used; every other endpoint is refused.
    LocalOnly,
}

impl ModelPolicy {
    /// Only `jp.` inference profiles (design §7).
    pub fn japan_only() -> Self {
        Self::AllowedPrefixes(vec![JAPAN_PROFILE_PREFIX.to_owned()])
    }

    /// Refuses `model` unless the policy lets it through.
    pub fn check(&self, model: &str) -> Result<(), ProviderError> {
        match self {
            Self::Any | Self::LocalOnly => Ok(()),
            Self::AllowedPrefixes(prefixes) if prefixes.iter().any(|p| model.starts_with(p)) => {
                Ok(())
            }
            Self::AllowedPrefixes(prefixes) => Err(ProviderError::PolicyViolation {
                model: model.to_owned(),
                reason: format!("the configuration allows only {}", quoted(prefixes)),
            }),
        }
    }

    /// Refuses an endpoint that is not local when the policy is
    /// [`ModelPolicy::LocalOnly`].
    pub fn check_endpoint(&self, model: &str, local: bool) -> Result<(), ProviderError> {
        match self {
            Self::LocalOnly if !local => Err(ProviderError::PolicyViolation {
                model: model.to_owned(),
                reason: "the configuration allows only a local provider".to_owned(),
            }),
            _ => Ok(()),
        }
    }
}

fn quoted(prefixes: &[String]) -> String {
    let list: Vec<String> = prefixes.iter().map(|p| format!("`{p}`")).collect();
    format!("model IDs starting with {}", list.join(", "))
}

/// `[residency]` of a run configuration (#592): where its code may go.
/// Without the section any model and endpoint is allowed, as before.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResidencyConfig {
    /// Every model ID must start with one of these (e.g. `["eu."]`).
    #[serde(default)]
    pub allowed_model_prefixes: Option<Vec<String>>,
    /// Every cloud backend (and every member of a `failover` chain) must
    /// declare a `region` in this list (e.g. `["ap-northeast-1", "asia-northeast1"]`,
    /// ADR 0018, #595).
    #[serde(default)]
    pub allowed_regions: Option<Vec<String>>,
    /// Only a `local` provider on the deployment's own network: its
    /// `base_url` host is loopback, a private or link-local address, a
    /// name with no dot (a container's), one under `.local`, `.internal`,
    /// `.lan`, `.localhost` or `.home.arpa`, or one of `local_hosts`.
    #[serde(default)]
    pub local_only: bool,
    /// More hosts `local_only` takes as local, e.g. `vllm.corp.example`:
    /// a name of the deployment's own DNS.
    #[serde(default)]
    pub local_hosts: Vec<String>,
}

/// Suffixes of names that only resolve on a private network.
const LOCAL_SUFFIXES: [&str; 5] = [".local", ".internal", ".lan", ".localhost", ".home.arpa"];

fn is_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                // fc00::/7, unique local; fe80::/10, link-local.
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

impl ResidencyConfig {
    /// The policy the section declares.
    pub fn policy(&self) -> Result<ModelPolicy, String> {
        match (&self.allowed_model_prefixes, self.local_only) {
            (Some(_), true) => {
                Err("[residency]: set `allowed_model_prefixes` or `local_only`, not both".into())
            }
            (Some(prefixes), false) if prefixes.is_empty() => Err(
                "[residency]: `allowed_model_prefixes` is empty, so no model could be used".into(),
            ),
            (Some(prefixes), false) if prefixes.iter().any(|p| p.trim().is_empty()) => {
                Err("[residency]: `allowed_model_prefixes` has an empty prefix".into())
            }
            (Some(prefixes), false) => Ok(ModelPolicy::AllowedPrefixes(prefixes.clone())),
            (None, true) => Ok(ModelPolicy::LocalOnly),
            (None, false) => Ok(ModelPolicy::Any),
        }
    }

    /// The policy, once `provider` and every model of `models` keep to it:
    /// checked when a configuration is loaded, so a wrong line stops the
    /// start instead of sending code elsewhere.
    pub fn check<'a>(
        &self,
        provider: &ProviderConfig,
        models: impl IntoIterator<Item = &'a str>,
    ) -> Result<ModelPolicy, String> {
        let policy = self.policy()?;
        if !self.local_hosts.is_empty() && !self.local_only {
            return Err("[residency]: `local_hosts` is only read with `local_only = true`".into());
        }
        if let Some(regions) = &self.allowed_regions {
            if self.local_only {
                return Err("[residency]: set `allowed_regions` or `local_only`, not both".into());
            }
            if regions.is_empty() || regions.iter().any(|r| r.trim().is_empty()) {
                return Err(
                    "[residency]: `allowed_regions` must contain non-empty region names".into(),
                );
            }
        }
        self.check_provider(provider, &policy, false)?;
        for model in models {
            policy
                .check(model)
                .map_err(|e| format!("[residency]: {e}"))?;
        }
        Ok(policy)
    }

    fn check_provider(
        &self,
        provider: &ProviderConfig,
        policy: &ModelPolicy,
        nested: bool,
    ) -> Result<(), String> {
        if let ProviderConfig::Failover { chain, .. } = provider {
            if nested {
                return Err(
                    "[residency]: nested `failover` inside `chain` is not supported".into(),
                );
            }
            if chain.is_empty() {
                return Err(
                    "[residency]: `failover` provider requires at least one backend in `chain`"
                        .into(),
                );
            }
            for item in chain {
                self.check_provider(item, policy, true)?;
            }
            return Ok(());
        }

        if *policy == ModelPolicy::LocalOnly {
            match provider {
                ProviderConfig::Local { base_url, .. } => self.check_local_url(base_url)?,
                _ => {
                    return Err(
                        "[residency]: `local_only` needs a `local` provider, not a cloud one"
                            .into(),
                    );
                }
            }
        }

        if let Some(allowed) = &self.allowed_regions {
            let region = match provider {
                ProviderConfig::Bedrock { region, .. }
                | ProviderConfig::VertexAi { region, .. } => Some(region.as_str()),
                ProviderConfig::OpenAiCompat { region, .. } => region.as_deref(),
                ProviderConfig::ClaudeApi { .. } | ProviderConfig::Local { .. } => None,
                ProviderConfig::Failover { .. } => unreachable!(),
            };
            match region {
                Some(r) if allowed.iter().any(|a| a.eq_ignore_ascii_case(r)) => {}
                Some(r) => {
                    return Err(format!(
                        "[residency]: provider region `{r}` is not in `allowed_regions` ({})",
                        allowed.join(", ")
                    ));
                }
                None => {
                    return Err(
                        "[residency]: `allowed_regions` requires every provider to declare an allowed `region`"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    /// Refuses a `base_url` whose host may be outside the deployment's
    /// network: a `local` provider can point anywhere.
    fn check_local_url(&self, base_url: &str) -> Result<(), String> {
        let refuse = |why: String| Err(format!("[residency]: `local_only`: {why}"));
        let url = match Url::parse(base_url) {
            Ok(url) => url,
            Err(e) => return refuse(format!("base_url `{base_url}`: {e}")),
        };
        let local = match url.host() {
            Some(Host::Ipv4(ip)) => is_local_ip(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => is_local_ip(IpAddr::V6(ip)),
            Some(Host::Domain(name)) => {
                let name = name.trim_end_matches('.').to_ascii_lowercase();
                name == "localhost"
                    || !name.contains('.')
                    || LOCAL_SUFFIXES.iter().any(|s| name.ends_with(s))
                    || self
                        .local_hosts
                        .iter()
                        .any(|h| h.trim_end_matches('.').eq_ignore_ascii_case(&name))
            }
            None => false,
        };
        if local {
            Ok(())
        } else {
            refuse(format!(
                "base_url `{base_url}` is not on a local network; list its host in `local_hosts` if it is"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn residency(text: &str) -> Result<ResidencyConfig, toml::de::Error> {
        toml::from_str(text)
    }

    fn local() -> ProviderConfig {
        local_at("http://localhost:11434/v1")
    }

    fn local_at(base_url: &str) -> ProviderConfig {
        ProviderConfig::Local {
            base_url: base_url.into(),
            api_key_env: None,
            protocol: crate::LocalProtocol::OpenAi,
        }
    }

    fn bedrock(region: &str) -> ProviderConfig {
        ProviderConfig::Bedrock {
            region: region.into(),
            require_jp_profile: false,
        }
    }

    #[test]
    fn any_allows_every_model() {
        assert!(ModelPolicy::Any.check("claude-sonnet-5-5").is_ok());
        assert!(ModelPolicy::Any.check_endpoint("m", false).is_ok());
    }

    #[test]
    fn japan_only_allows_jp_profiles() {
        assert!(
            ModelPolicy::japan_only()
                .check("jp.anthropic.claude-opus-5-5")
                .is_ok()
        );
    }

    #[test]
    fn japan_only_refuses_global_and_bare_ids() {
        for model in [
            "claude-sonnet-5-5",
            "global.anthropic.claude-sonnet-5-5",
            "anthropic.claude-haiku-5-5",
            "JP.anthropic.claude-opus-5-5",
            "",
        ] {
            let err = ModelPolicy::japan_only().check(model).unwrap_err();
            assert!(
                matches!(err, ProviderError::PolicyViolation { .. }),
                "{model}"
            );
        }
    }

    #[test]
    fn prefixes_allow_any_listed_region() {
        let policy = ModelPolicy::AllowedPrefixes(vec!["eu.".into(), "us.".into()]);
        assert!(policy.check("eu.anthropic.claude-sonnet-5-5").is_ok());
        assert!(policy.check("us.anthropic.claude-sonnet-5-5").is_ok());
        let err = policy
            .check("apac.anthropic.claude-sonnet-5-5")
            .unwrap_err();
        assert!(err.to_string().contains("`eu.`, `us.`"), "{err}");
    }

    #[test]
    fn local_only_refuses_any_endpoint_that_is_not_local() {
        assert!(ModelPolicy::LocalOnly.check("qwen2.5-coder:32b").is_ok());
        assert!(ModelPolicy::LocalOnly.check_endpoint("m", true).is_ok());
        let err = ModelPolicy::LocalOnly
            .check_endpoint("claude-sonnet-5-5", false)
            .unwrap_err();
        assert!(matches!(err, ProviderError::PolicyViolation { .. }));
    }

    #[test]
    fn no_section_is_any() {
        assert_eq!(
            ResidencyConfig::default().policy().unwrap(),
            ModelPolicy::Any
        );
    }

    #[test]
    fn the_section_is_read() {
        let eu = residency("allowed_model_prefixes = [\"eu.\"]").unwrap();
        assert_eq!(
            eu.policy().unwrap(),
            ModelPolicy::AllowedPrefixes(vec!["eu.".into()])
        );
        let airgap = residency("local_only = true").unwrap();
        assert_eq!(airgap.policy().unwrap(), ModelPolicy::LocalOnly);
        assert!(residency("local = true").is_err(), "unknown fields refused");
    }

    #[test]
    fn a_section_that_allows_nothing_or_both_is_refused() {
        for (text, says) in [
            ("allowed_model_prefixes = []", "empty"),
            ("allowed_model_prefixes = [\" \"]", "empty prefix"),
            (
                "allowed_model_prefixes = [\"eu.\"]\nlocal_only = true",
                "not both",
            ),
        ] {
            let err = residency(text).unwrap().policy().unwrap_err();
            assert!(err.contains(says), "{text}: {err}");
        }
    }

    #[test]
    fn a_model_outside_the_prefixes_is_refused_at_load() {
        let eu = residency("allowed_model_prefixes = [\"eu.\"]").unwrap();
        let ok = eu.check(&bedrock("eu-central-1"), ["eu.anthropic.claude-sonnet-5-5"]);
        assert!(ok.is_ok());
        let err = eu
            .check(
                &bedrock("eu-central-1"),
                [
                    "eu.anthropic.claude-haiku-5-5",
                    "global.anthropic.claude-opus-5-5",
                ],
            )
            .unwrap_err();
        assert!(err.contains("global.anthropic.claude-opus-5-5"), "{err}");
    }

    #[test]
    fn local_only_needs_a_local_provider() {
        let airgap = residency("local_only = true").unwrap();
        assert!(airgap.check(&local(), ["qwen2.5-coder:32b"]).is_ok());
        let err = airgap
            .check(&bedrock("eu-central-1"), ["eu.anthropic.claude-haiku-5-5"])
            .unwrap_err();
        assert!(err.contains("local"), "{err}");
    }

    #[test]
    fn local_only_takes_only_hosts_on_the_deployments_network() {
        let airgap = residency("local_only = true").unwrap();
        for url in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8000/v1",
            "http://10.0.3.7:8000/v1",
            "http://192.168.1.20/v1",
            "http://[::1]:8000/v1",
            "http://[fd12::7]:8000/v1",
            "http://ollama:11434/v1",
            "http://vllm.internal/v1",
            "http://gpu1.lan:8000/v1",
        ] {
            assert!(airgap.check(&local_at(url), ["m"]).is_ok(), "{url}");
        }
        for url in [
            "https://api.openai.com/v1",
            "http://8.8.8.8/v1",
            "http://vllm.corp.example/v1",
            "not a url",
        ] {
            let err = airgap.check(&local_at(url), ["m"]).unwrap_err();
            assert!(err.contains("local_only"), "{url}: {err}");
        }
        let listed = residency("local_only = true\nlocal_hosts = [\"vllm.corp.example\"]").unwrap();
        assert!(
            listed
                .check(&local_at("http://VLLM.corp.example/v1"), ["m"])
                .is_ok()
        );
        let stray = residency("local_hosts = [\"vllm.corp.example\"]").unwrap();
        let err = stray.check(&local(), ["m"]).unwrap_err();
        assert!(err.contains("local_hosts"), "{err}");
    }

    #[test]
    fn failover_chain_checks_allowed_regions_for_every_member() {
        let jp = residency("allowed_regions = [\"ap-northeast-1\", \"asia-northeast1\"]").unwrap();
        let valid_chain = ProviderConfig::Failover {
            retryable_only: true,
            chain: vec![
                bedrock("ap-northeast-1"),
                ProviderConfig::VertexAi {
                    project: "acme".into(),
                    region: "asia-northeast1".into(),
                    access_token_env: "GCP_TOKEN".into(),
                    base_url: None,
                },
            ],
        };
        assert!(jp.check(&valid_chain, ["gemini-2.5-pro"]).is_ok());

        let leaking_chain = ProviderConfig::Failover {
            retryable_only: true,
            chain: vec![
                bedrock("ap-northeast-1"),
                ProviderConfig::VertexAi {
                    project: "acme".into(),
                    region: "us-central1".into(),
                    access_token_env: "GCP_TOKEN".into(),
                    base_url: None,
                },
            ],
        };
        let err = jp.check(&leaking_chain, ["gemini-2.5-pro"]).unwrap_err();
        assert!(err.contains("us-central1"), "{err}");
    }

    #[test]
    fn local_only_refuses_failover_chain_with_any_cloud_backend() {
        let airgap = residency("local_only = true").unwrap();
        let mixed = ProviderConfig::Failover {
            retryable_only: true,
            chain: vec![
                local(),
                ProviderConfig::OpenAiCompat {
                    base_url: "https://api.openai.com/v1".into(),
                    api_key_env: "OPENAI_API_KEY".into(),
                    azure_api_version: None,
                    region: Some("us-east".into()),
                },
            ],
        };
        let err = airgap.check(&mixed, ["m"]).unwrap_err();
        assert!(err.contains("local_only"), "{err}");
    }
}
