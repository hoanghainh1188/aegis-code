//! AWS Secrets Manager (ADR 0003 §6): secret `X` is the secret named
//! `<prefix>X`, its `SecretString`. Credentials come from the standard
//! chain, on EC2 the instance role: no key of ours is stored anywhere.

use aws_sdk_secretsmanager::error::DisplayErrorContext;

use crate::source::{Fetch, SecretSource};

#[derive(Debug, Clone)]
pub struct AwsSource {
    client: aws_sdk_secretsmanager::Client,
    region: String,
    prefix: String,
}

impl AwsSource {
    /// Secrets Manager of `region`, with the standard credential chain.
    pub async fn new(region: &str, prefix: &str) -> Self {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.to_owned()))
            .load()
            .await;
        Self::with_config(
            aws_sdk_secretsmanager::config::Builder::from(&config).build(),
            prefix,
        )
    }

    /// With a client configuration of the caller's (tests: a local
    /// endpoint).
    pub fn with_config(config: aws_sdk_secretsmanager::Config, prefix: &str) -> Self {
        let region = config
            .region()
            .map_or_else(|| "no region".to_owned(), ToString::to_string);
        Self {
            client: aws_sdk_secretsmanager::Client::from_conf(config),
            region,
            prefix: prefix.to_owned(),
        }
    }

    pub fn region(&self) -> &str {
        &self.region
    }
}

impl SecretSource for AwsSource {
    fn describe(&self) -> String {
        format!("AWS Secrets Manager ({}, `{}…`)", self.region, self.prefix)
    }

    fn fetch<'a>(&'a self, name: &'a str) -> Fetch<'a> {
        Box::pin(async move {
            let read = self
                .client
                .get_secret_value()
                .secret_id(format!("{}{name}", self.prefix))
                .send()
                .await;
            match read {
                Ok(output) => match output.secret_string() {
                    Some(value) => Ok(Some(value.to_owned())),
                    None => Err("the secret is binary, not a string".into()),
                },
                Err(e)
                    if e.as_service_error()
                        .is_some_and(|e| e.is_resource_not_found_exception()) =>
                {
                    Ok(None)
                }
                Err(e) => Err(DisplayErrorContext(&e).to_string()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_secretsmanager::config::{BehaviorVersion, Credentials, Region};
    use wiremock::matchers::{body_string_contains, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn source(server: &MockServer) -> AwsSource {
        let config = aws_sdk_secretsmanager::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("ap-northeast-1"))
            .credentials_provider(Credentials::new("test", "test", None, None, "test"))
            .endpoint_url(server.uri())
            .build();
        AwsSource::with_config(config, "aegis/staging/")
    }

    fn target() -> wiremock::matchers::HeaderExactMatcher {
        header("x-amz-target", "secretsmanager.GetSecretValue")
    }

    #[tokio::test]
    async fn reads_the_secret_string_under_the_prefix() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(target())
            .and(body_string_contains("aegis/staging/ANTHROPIC_API_KEY"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"Name":"aegis/staging/ANTHROPIC_API_KEY","SecretString":"sk-test"}"#,
                "application/x-amz-json-1.1",
            ))
            .mount(&server)
            .await;
        let source = source(&server);
        assert_eq!(source.region(), "ap-northeast-1");
        assert!(source.describe().contains("ap-northeast-1"));
        assert_eq!(
            source.fetch("ANTHROPIC_API_KEY").await,
            Ok(Some("sk-test".into()))
        );
    }

    #[tokio::test]
    async fn a_secret_that_does_not_exist_is_none_other_errors_fail() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("MISSING"))
            .respond_with(ResponseTemplate::new(400).set_body_raw(
                r#"{"__type":"ResourceNotFoundException","message":"not found"}"#,
                "application/x-amz-json-1.1",
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("DENIED"))
            .respond_with(ResponseTemplate::new(400).set_body_raw(
                r#"{"__type":"AccessDeniedException","message":"no"}"#,
                "application/x-amz-json-1.1",
            ))
            .mount(&server)
            .await;
        let source = source(&server);
        assert_eq!(source.fetch("MISSING").await, Ok(None));
        let err = source.fetch("DENIED").await.unwrap_err();
        assert!(err.contains("AccessDenied"), "{err}");
    }
}
