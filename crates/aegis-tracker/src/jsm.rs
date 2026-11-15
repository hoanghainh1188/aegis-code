//! Jira Service Management (JSM) ITSM connector (#453): polls requests with
//! mapped severity (`P1`–`P3`) and SLA deadlines, reads request context & log
//! attachments, and writes internal/public Service Desk updates + transitions.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::config::JsmConfig;
use crate::http;
use crate::jira::{JiraRest, message};
use crate::ticket::{Priority, SearchTicket, checked_key, parse_deadline};
use crate::{Report, Ticket, TrackerError};

const JSM: &str = "JSM";

#[derive(Debug, Clone)]
pub struct Jsm {
    rest: JiraRest,
    jql: String,
    sla_field: Option<String>,
    pr_transition: Option<String>,
    diagnosis_transition: Option<String>,
    internal_comments: bool,
}

fn search_fields(sla_field: Option<&str>) -> String {
    match sla_field {
        Some(f) => format!("summary,priority,duedate,{f}"),
        None => "summary,priority,duedate".to_owned(),
    }
}

fn issue_fields(sla_field: Option<&str>) -> String {
    match sla_field {
        Some(f) => format!("summary,description,attachment,priority,duedate,{f}"),
        None => "summary,description,attachment,priority,duedate".to_owned(),
    }
}

fn extract_priority(fields: &Value) -> Option<Priority> {
    let raw = fields
        .get("priority")
        .and_then(|p| p.get("name").and_then(Value::as_str).or_else(|| p.as_str()))?;
    Priority::parse_label(raw)
}

fn parse_jsm_sla_value(val: &Value) -> Option<DateTime<Utc>> {
    if let Some(s) = val.as_str() {
        return parse_deadline(s);
    }
    if let Some(iso) = val
        .pointer("/ongoingCycle/breachTime/iso8601")
        .and_then(Value::as_str)
    {
        return parse_deadline(iso);
    }
    if let Some(iso) = val.pointer("/breachTime/iso8601").and_then(Value::as_str) {
        return parse_deadline(iso);
    }
    if let Some(last) = val
        .get("completedCycles")
        .and_then(Value::as_array)
        .and_then(|arr| arr.last())
        && let Some(iso) = last.pointer("/breachTime/iso8601").and_then(Value::as_str)
    {
        return parse_deadline(iso);
    }
    None
}

fn extract_sla_deadline(fields: &Value, sla_field: Option<&str>) -> Option<DateTime<Utc>> {
    if let Some(field) = sla_field
        && let Some(val) = fields.get(field)
        && let Some(dt) = parse_jsm_sla_value(val)
    {
        return Some(dt);
    }
    fields
        .get("duedate")
        .and_then(Value::as_str)
        .and_then(parse_deadline)
}

impl Jsm {
    pub(crate) fn new(config: &JsmConfig, token: &str) -> Result<Self, TrackerError> {
        Ok(Self {
            rest: JiraRest::new(
                JSM,
                &config.url,
                config.deployment,
                config.user.as_deref(),
                token,
            )?,
            jql: config.jql.clone(),
            sla_field: config.sla_field.clone(),
            pr_transition: config.pr_transition.clone(),
            diagnosis_transition: config.diagnosis_transition.clone(),
            internal_comments: config.internal_comments,
        })
    }

    fn servicedesk_api(&self, rest: &str) -> String {
        format!("{}/rest/servicedeskapi/{rest}", self.rest.site)
    }

    pub(crate) async fn search_tickets(&self) -> Result<Vec<SearchTicket>, TrackerError> {
        let fields = search_fields(self.sla_field.as_deref());
        let issues = self.rest.search_issues(&self.jql, &fields).await?;
        let mut found = Vec::with_capacity(issues.len());
        for issue in issues {
            let Some(key) = issue.get("key").and_then(Value::as_str) else {
                continue;
            };
            let fields_val = issue.get("fields").unwrap_or(&Value::Null);
            found.push(SearchTicket {
                key: key.to_owned(),
                priority: extract_priority(fields_val),
                sla_deadline: extract_sla_deadline(fields_val, self.sla_field.as_deref()),
            });
        }
        Ok(found)
    }

    pub(crate) async fn ticket(&self, key: &str) -> Result<Ticket, TrackerError> {
        checked_key(key)?;
        let fields = issue_fields(self.sla_field.as_deref());
        let issue = self.rest.fetch_issue(key, &fields).await?;
        let logs = self
            .rest
            .download_logs(issue.fields.get("attachment"))
            .await?;
        Ok(Ticket {
            url: format!("{}/browse/{}", self.rest.site, issue.key),
            key: issue.key,
            title: issue
                .fields
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            body: issue
                .fields
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            logs,
            priority: extract_priority(&issue.fields),
            sla_deadline: extract_sla_deadline(&issue.fields, self.sla_field.as_deref()),
        })
    }

    pub(crate) async fn report(&self, key: &str, report: &Report<'_>) -> Result<(), TrackerError> {
        checked_key(key)?;
        let comment_url = self.servicedesk_api(&format!("request/{key}/comment"));
        let comment_body = json!({
            "body": report.comment,
            "public": !self.internal_comments,
        });
        http::send(JSM, self.rest.post(&comment_url, &comment_body), message).await?;

        if let Some(pr) = report.pr_url {
            self.rest.post_remotelink(key, pr).await?;
            if let Some(name) = &self.pr_transition {
                self.rest.transition(key, name).await?;
            }
        } else if let Some(name) = &self.diagnosis_transition {
            self.rest.transition(key, name).await?;
        }
        Ok(())
    }
}
