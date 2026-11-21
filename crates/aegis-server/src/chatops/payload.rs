//! A [`ChatMessage`] as each webhook takes it: Slack's Block Kit, Teams'
//! Adaptive Card. Every text in the message is already made inert
//! ([`super::message`]); Slack's mrkdwn is escaped on top of that, as its
//! own markup asks (`&`, `<`, `>`).

use serde_json::{Value, json};

use super::ChatKind;
use super::message::ChatMessage;

/// Slack's limits for a header's text, a section's and a field's.
const SLACK_HEADER_CHARS: usize = 150;
const SLACK_SECTION_CHARS: usize = 3000;
const SLACK_FIELD_CHARS: usize = 2000;

/// The JSON body for `kind`.
pub fn payload(kind: ChatKind, message: &ChatMessage) -> Value {
    match kind {
        ChatKind::Slack => slack(message),
        ChatKind::Teams => teams(message),
    }
}

/// `&`, `<` and `>` as Slack's mrkdwn wants them: nothing becomes a link,
/// a mention or `<!channel>`.
pub fn slack_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// Block Kit: a header, the summary, the facts as fields, the link as a
/// button, the job in the context line. `text` is the notification's
/// fallback.
pub fn slack(message: &ChatMessage) -> Value {
    let mut blocks = vec![json!({
        "type": "header",
        "text": {
            "type": "plain_text",
            "text": truncate(&message.title, SLACK_HEADER_CHARS),
            "emoji": false,
        },
    })];
    if let Some(summary) = &message.summary {
        blocks.push(json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": truncate(&slack_escape(summary), SLACK_SECTION_CHARS),
            },
        }));
    }
    if !message.facts.is_empty() {
        // Slack takes at most ten fields in a section.
        let fields: Vec<Value> = message
            .facts
            .iter()
            .take(10)
            .map(|(name, value)| {
                json!({
                    "type": "mrkdwn",
                    "text": truncate(
                        &format!("*{}*\n{}", slack_escape(name), slack_escape(value)),
                        SLACK_FIELD_CHARS,
                    ),
                })
            })
            .collect();
        blocks.push(json!({ "type": "section", "fields": fields }));
    }
    if let Some(link) = &message.link {
        blocks.push(json!({
            "type": "actions",
            "elements": [{
                "type": "button",
                "text": { "type": "plain_text", "text": link.label },
                "url": link.url,
            }],
        }));
    }
    blocks.push(json!({
        "type": "context",
        "elements": [{ "type": "mrkdwn", "text": slack_escape(&message.footer) }],
    }));
    json!({
        "text": slack_escape(&message.title),
        "blocks": blocks,
        // No link or mention is unfurled or parsed from the text.
        "unfurl_links": false,
        "unfurl_media": false,
    })
}

/// An Adaptive Card in a message, as both Teams' Incoming Webhooks and
/// Power Automate's Workflows take it. Teams pings only who a card's
/// `msteams.entities` names, and this card names no one.
pub fn teams(message: &ChatMessage) -> Value {
    let mut body = vec![json!({
        "type": "TextBlock",
        "text": message.title,
        "weight": "Bolder",
        "size": "Medium",
        "wrap": true,
    })];
    if let Some(summary) = &message.summary {
        body.push(json!({ "type": "TextBlock", "text": summary, "wrap": true }));
    }
    if !message.facts.is_empty() {
        let facts: Vec<Value> = message
            .facts
            .iter()
            .map(|(name, value)| json!({ "title": name, "value": value }))
            .collect();
        body.push(json!({ "type": "FactSet", "facts": facts }));
    }
    body.push(json!({
        "type": "TextBlock",
        "text": message.footer,
        "isSubtle": true,
        "size": "Small",
        "wrap": true,
    }));
    let mut card = json!({
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "type": "AdaptiveCard",
        "version": "1.4",
        "body": body,
    });
    if let Some(link) = &message.link {
        card["actions"] = json!([{
            "type": "Action.OpenUrl",
            "title": link.label,
            "url": link.url,
        }]);
    }
    json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "contentUrl": null,
            "content": card,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatops::ChatEvent;
    use crate::chatops::message::Link;

    fn message() -> ChatMessage {
        ChatMessage {
            event: ChatEvent::PrOpened,
            title: "aegis-code opened a pull request in acme-api".into(),
            summary: Some("fixed <it> & more".into()),
            facts: vec![
                ("Cost".into(), "$1.20".into()),
                ("Gate".into(), "passed".into()),
            ],
            link: Some(Link {
                label: "Open pull request",
                url: "https://github.com/acme/api/pull/7".into(),
            }),
            footer: "aegis-code job 42".into(),
        }
    }

    #[test]
    fn slack_gets_block_kit_with_its_markup_escaped() {
        let p = slack(&message());
        assert_eq!(p["text"], "aegis-code opened a pull request in acme-api");
        let blocks = p["blocks"].as_array().unwrap();
        let kinds: Vec<&str> = blocks.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            ["header", "section", "section", "actions", "context"]
        );
        assert_eq!(blocks[0]["text"]["type"], "plain_text");
        assert_eq!(blocks[1]["text"]["text"], "fixed &lt;it&gt; &amp; more");
        assert_eq!(blocks[2]["fields"][0]["text"], "*Cost*\n$1.20");
        assert_eq!(
            blocks[3]["elements"][0]["url"],
            "https://github.com/acme/api/pull/7"
        );
        assert_eq!(p["unfurl_links"], false);
    }

    #[test]
    fn slack_leaves_out_what_the_message_has_not() {
        let m = ChatMessage {
            summary: None,
            facts: vec![],
            link: None,
            ..message()
        };
        let p = slack(&m);
        let kinds: Vec<&str> = p["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["type"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["header", "context"]);
    }

    #[test]
    fn slack_headers_are_cut_to_its_limit() {
        let m = ChatMessage {
            title: "x".repeat(400),
            ..message()
        };
        let header = slack(&m)["blocks"][0]["text"]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(header.chars().count(), SLACK_HEADER_CHARS);
        assert!(header.ends_with('…'));
    }

    #[test]
    fn teams_gets_an_adaptive_card_in_a_message() {
        let p = teams(&message());
        assert_eq!(p["type"], "message");
        let attachment = &p["attachments"][0];
        assert_eq!(
            attachment["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        let card = &attachment["content"];
        assert_eq!(card["type"], "AdaptiveCard");
        assert_eq!(card["version"], "1.4");
        assert_eq!(card["body"][0]["weight"], "Bolder");
        assert_eq!(card["body"][2]["type"], "FactSet");
        assert_eq!(card["body"][2]["facts"][1]["title"], "Gate");
        assert_eq!(card["actions"][0]["type"], "Action.OpenUrl");
        assert!(card.get("msteams").is_none(), "no one is mentioned");
    }

    #[test]
    fn teams_has_no_actions_without_a_link() {
        let m = ChatMessage {
            link: None,
            ..message()
        };
        assert!(
            teams(&m)["attachments"][0]["content"]
                .get("actions")
                .is_none()
        );
    }
}
