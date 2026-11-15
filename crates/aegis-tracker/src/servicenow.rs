//! ServiceNow ITSM Table API connector (#453): polls incidents/tasks with
//! mapped severity (`P1`–`P3`) and SLA deadlines, reads descriptions & log
//! attachments, and writes work notes/comments + state updates on completion.

use chrono::{DateTime, Utc};
use reqwest::RequestBuilder;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::config::{ServiceNowConfig, site_url};
use crate::http::{self, client};
use crate::ticket::{
    self, Log, MAX_LOG_BYTES, MAX_LOGS, Priority, SearchTicket, checked_servicenow_key,
    parse_deadline,
};
use crate::{MAX_TICKETS, Report, Ticket, TrackerError};

const SERVICENOW: &str = "ServiceNow";
const PAGE: usize = 100;
const SEARCH_FIELDS: &str =
    "sys_id,number,short_description,priority,urgency,impact,sla_due,due_date";
const DETAIL_FIELDS: &str =
    "sys_id,number,short_description,description,priority,urgency,impact,sla_due,due_date";

#[derive(Debug, Clone)]
pub struct ServiceNow {
    client: reqwest::Client,
    site: String,
    user: Option<String>,
    token: String,
    table: String,
    query: String,
    pr_state: Option<String>,
    diagnosis_state: Option<String>,
    work_notes: bool,
}

#[derive(Debug, Deserialize)]
struct TableResponse {
    #[serde(default)]
    result: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct AttachmentRow {
    sys_id: String,
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default, deserialize_with = "de_size")]
    size_bytes: u64,
}

fn de_size<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let val = Value::deserialize(d)?;
    if let Some(n) = val.as_u64() {
        return Ok(n);
    }
    if let Some(s) = val.as_str()
        && let Ok(n) = s.trim().parse::<u64>()
    {
        return Ok(n);
    }
    Ok(0)
}

#[derive(Debug, Deserialize)]
struct AttachmentList {
    #[serde(default)]
    result: Vec<AttachmentRow>,
}

/// ServiceNow error payload `{ "error": { "message": "...", "detail": "..." } }`.
fn message(v: &Value) -> Option<String> {
    let err = v.get("error")?;
    let mut parts = Vec::new();
    if let Some(m) = err.get("message").and_then(Value::as_str)
        && !m.trim().is_empty()
    {
        parts.push(m.trim().to_owned());
    }
    if let Some(d) = err.get("detail").and_then(Value::as_str)
        && !d.trim().is_empty()
    {
        parts.push(d.trim().to_owned());
    }
    (!parts.is_empty()).then(|| parts.join(": "))
}

/// Read either a flat JSON string or ServiceNow `{ value, display_value }` wrapper.
fn sn_value<'a>(record: &'a Value, field: &str) -> Option<&'a str> {
    let v = record.get(field)?;
    if let Some(s) = v.as_str() {
        let t = s.trim();
        return (!t.is_empty()).then_some(t);
    }
    if let Some(s) = v.get("value").and_then(Value::as_str) {
        let t = s.trim();
        if !t.is_empty() {
            return Some(t);
        }
    }
    if let Some(s) = v.get("display_value").and_then(Value::as_str) {
        let t = s.trim();
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

fn sn_display<'a>(record: &'a Value, field: &str) -> Option<&'a str> {
    let v = record.get(field)?;
    if let Some(s) = v.as_str() {
        let t = s.trim();
        return (!t.is_empty()).then_some(t);
    }
    if let Some(s) = v.get("display_value").and_then(Value::as_str) {
        let t = s.trim();
        if !t.is_empty() {
            return Some(t);
        }
    }
    if let Some(s) = v.get("value").and_then(Value::as_str) {
        let t = s.trim();
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

fn leading_tier(raw: &str) -> Option<u8> {
    match raw.trim().bytes().next() {
        Some(b'1') => Some(1),
        Some(b'2') => Some(2),
        Some(b'3') => Some(3),
        _ => None,
    }
}

fn extract_priority(record: &Value) -> Option<Priority> {
    if let Some(p) = record.get("priority") {
        for candidate in [
            p.as_str(),
            p.get("value").and_then(Value::as_str),
            p.get("display_value").and_then(Value::as_str),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(mapped) = Priority::parse_label(candidate) {
                return Some(mapped);
            }
        }
    }
    // Fall back to ServiceNow's standard ITIL Impact × Urgency matrix.
    let impact = sn_value(record, "impact").and_then(leading_tier);
    let urgency = sn_value(record, "urgency").and_then(leading_tier);
    match (impact, urgency) {
        (Some(1), Some(1)) => Some(Priority::P1),
        (Some(1), Some(_)) | (Some(_), Some(1)) | (Some(2), Some(2)) => Some(Priority::P2),
        (Some(2 | 3), Some(2 | 3)) => Some(Priority::P3),
        _ => None,
    }
}

fn extract_sla_deadline(record: &Value) -> Option<DateTime<Utc>> {
    for field in ["sla_due", "due_date"] {
        let Some(v) = record.get(field) else {
            continue;
        };
        for candidate in [
            v.as_str(),
            v.get("value").and_then(Value::as_str),
            v.get("display_value").and_then(Value::as_str),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(dt) = parse_deadline(candidate) {
                return Some(dt);
            }
        }
    }
    None
}

impl ServiceNow {
    pub(crate) fn new(config: &ServiceNowConfig, token: &str) -> Result<Self, TrackerError> {
        Ok(Self {
            client: client(SERVICENOW)?,
            site: site_url(&config.url)?.to_owned(),
            user: config
                .user
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_owned),
            token: token.to_owned(),
            table: config.table.clone(),
            query: config.query.clone(),
            pr_state: config.pr_state.clone(),
            diagnosis_state: config.diagnosis_state.clone(),
            work_notes: config.work_notes,
        })
    }

    fn table_api(&self, suffix: &str) -> String {
        if suffix.is_empty() {
            format!("{}/api/now/table/{}", self.site, self.table)
        } else {
            format!("{}/api/now/table/{}/{suffix}", self.site, self.table)
        }
    }

    fn authed(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.user {
            Some(user) => request.basic_auth(user, Some(&self.token)),
            None => request.bearer_auth(&self.token),
        }
    }

    fn get(&self, url: &str) -> RequestBuilder {
        self.authed(self.client.get(url))
            .header("Accept", "application/json")
    }

    fn patch(&self, url: &str, body: &Value) -> RequestBuilder {
        self.authed(self.client.patch(url))
            .header("Accept", "application/json")
            .json(body)
    }

    pub(crate) async fn search_tickets(&self) -> Result<Vec<SearchTicket>, TrackerError> {
        let mut found = Vec::new();
        let mut offset = 0usize;
        loop {
            let limit = PAGE.min(MAX_TICKETS.saturating_sub(found.len()));
            if limit == 0 {
                break;
            }
            let limit_str = limit.to_string();
            let offset_str = offset.to_string();
            let request = self.get(&self.table_api("")).query(&[
                ("sysparm_query", self.query.as_str()),
                ("sysparm_fields", SEARCH_FIELDS),
                ("sysparm_limit", limit_str.as_str()),
                ("sysparm_offset", offset_str.as_str()),
                ("sysparm_exclude_reference_link", "true"),
            ]);
            let page: TableResponse = http::json(SERVICENOW, request, message).await?;
            let got = page.result.len();
            for row in page.result {
                let Some(key) = sn_value(&row, "number") else {
                    continue;
                };
                if checked_servicenow_key(key).is_err() {
                    continue;
                }
                found.push(SearchTicket {
                    key: key.to_owned(),
                    priority: extract_priority(&row),
                    sla_deadline: extract_sla_deadline(&row),
                });
            }
            offset += got;
            if got < limit || found.len() >= MAX_TICKETS {
                break;
            }
        }
        found.truncate(MAX_TICKETS);
        Ok(found)
    }

    async fn fetch_by_number(&self, number: &str, fields: &str) -> Result<Value, TrackerError> {
        checked_servicenow_key(number)?;
        let query = format!("number={number}");
        let request = self.get(&self.table_api("")).query(&[
            ("sysparm_query", query.as_str()),
            ("sysparm_fields", fields),
            ("sysparm_limit", "1"),
            ("sysparm_exclude_reference_link", "true"),
        ]);
        let page: TableResponse = http::json(SERVICENOW, request, message).await?;
        page.result.into_iter().next().ok_or_else(|| {
            TrackerError::Config(format!(
                "servicenow: ticket `{number}` not found in `{}`",
                self.table
            ))
        })
    }

    async fn download_logs(&self, table_sys_id: &str) -> Result<Vec<Log>, TrackerError> {
        let query = format!("table_sys_id={table_sys_id}");
        let url = format!("{}/api/now/attachment", self.site);
        let request = self
            .get(&url)
            .query(&[("sysparm_query", query.as_str()), ("sysparm_limit", "10")]);
        let list: AttachmentList = match http::json(SERVICENOW, request, message).await {
            Ok(list) => list,
            // Attachment endpoint is optional in minimal configurations / mocks.
            Err(TrackerError::Api {
                status: 403 | 404, ..
            }) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut logs = Vec::new();
        for a in &list.result {
            if logs.len() == MAX_LOGS {
                break;
            }
            if !ticket::is_log(&a.file_name, a.content_type.as_deref(), a.size_bytes) {
                continue;
            }
            let file_url = format!("{}/api/now/attachment/{}/file", self.site, a.sys_id);
            let response = http::send(SERVICENOW, self.get(&file_url), message).await?;
            let (bytes, cut) = http::read(SERVICENOW, response, MAX_LOG_BYTES).await?;
            logs.push(ticket::log(&a.file_name, &bytes, cut));
        }
        Ok(logs)
    }

    pub(crate) async fn ticket(&self, key: &str) -> Result<Ticket, TrackerError> {
        let record = self.fetch_by_number(key, DETAIL_FIELDS).await?;
        let sys_id = sn_value(&record, "sys_id").ok_or_else(|| {
            TrackerError::Config(format!("servicenow: `{key}` returned no sys_id"))
        })?;
        let logs = self.download_logs(sys_id).await?;
        let number = sn_value(&record, "number").unwrap_or(key).to_owned();
        Ok(Ticket {
            url: format!(
                "{}/nav_to.do?uri={}.do?sys_id={sys_id}",
                self.site, self.table
            ),
            key: number,
            title: sn_display(&record, "short_description")
                .unwrap_or_default()
                .to_owned(),
            body: sn_display(&record, "description")
                .unwrap_or_default()
                .to_owned(),
            logs,
            priority: extract_priority(&record),
            sla_deadline: extract_sla_deadline(&record),
        })
    }

    pub(crate) async fn report(&self, key: &str, report: &Report<'_>) -> Result<(), TrackerError> {
        let record = self.fetch_by_number(key, "sys_id").await?;
        let sys_id = sn_value(&record, "sys_id").ok_or_else(|| {
            TrackerError::Config(format!("servicenow: `{key}` returned no sys_id"))
        })?;
        let note = match report.pr_url {
            Some(pr) if !report.comment.contains(pr) => {
                format!("{}\n\nPull Request: {pr}", report.comment)
            }
            _ => report.comment.to_owned(),
        };
        let mut body = Map::new();
        let note_field = if self.work_notes {
            "work_notes"
        } else {
            "comments"
        };
        body.insert(note_field.to_owned(), Value::String(note));
        let next_state = if report.pr_url.is_some() {
            &self.pr_state
        } else {
            &self.diagnosis_state
        };
        if let Some(state) = next_state {
            body.insert("state".to_owned(), Value::String(state.clone()));
        }
        let patch_url = self.table_api(sys_id);
        http::send(
            SERVICENOW,
            self.patch(&patch_url, &Value::Object(body)),
            message,
        )
        .await?;
        Ok(())
    }
}
