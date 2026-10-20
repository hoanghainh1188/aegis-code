//! "Xong khi" of #13: the tool list (BP1) and the system blocks (BP2) are
//! byte-identical for two different issues on the same repo, and the
//! module map is built once and then read back.

mod common;

use std::collections::VecDeque;
use std::sync::Mutex;

use aegis_aci::{AciError, load_or_create_summary, module_summary};
use aegis_lang::{Language, ProfileOverrides, Profiles};
use aegis_orchestrator::{
    LoopConfig, MaxModelCalls, McpServerSpec, McpTools, ModelSpec, RepoPrompt, Session, run_loop,
    system_blocks,
};
use aegis_provider::{
    CacheTtl, ContentBlock, Message, ModelProvider, ProviderError, Request, Response, StopReason,
    Usage,
};
use common::{Scratch, profile};
use serde_json::json;

struct Recorder {
    script: Mutex<VecDeque<Response>>,
    requests: Mutex<Vec<Request>>,
}

fn response(content: Vec<ContentBlock>, stop: StopReason) -> Response {
    Response {
        id: "msg".into(),
        model: "claude-sonnet-5-5".into(),
        content,
        stop_reason: Some(stop),
        usage: Usage::default(),
    }
}

impl Recorder {
    /// One `search_symbol` call, then the end of the turn.
    fn new() -> Self {
        let call = ContentBlock::ToolUse {
            id: "t1".into(),
            name: "search_symbol".into(),
            input: json!({"name": "Cart"}),
        };
        Self {
            script: Mutex::new(VecDeque::from([
                response(vec![call], StopReason::ToolUse),
                response(vec![ContentBlock::text("{}")], StopReason::EndTurn),
            ])),
            requests: Mutex::new(vec![]),
        }
    }
}

impl ModelProvider for Recorder {
    async fn send(&self, request: &Request) -> Result<Response, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .expect("script exhausted"))
    }
}

fn model() -> ModelSpec {
    ModelSpec {
        id: "claude-sonnet-5-5".into(),
        effort: None,
        max_tokens: 4096,
        context_window: 1_000_000,
    }
}

#[test]
fn the_module_map_names_the_main_definitions() {
    let repo = Scratch::fixture("rust");
    let aci = repo.aci(profile(Language::Rust, &ProfileOverrides::default()));
    let map = module_summary(aci.workspace()).unwrap();
    assert!(map.starts_with("# Module map\n"), "{map}");
    assert!(
        map.ends_with("\n- `src` (1 files): Cart, CartError, Priced, Total, cart\n"),
        "{map}"
    );
    assert_eq!(
        module_summary(aci.workspace()).unwrap(),
        map,
        "deterministic"
    );
}

#[test]
fn the_module_map_is_built_once_and_kept_outside_the_repo() {
    let repo = Scratch::fixture("rust");
    let aci = repo.aci(profile(Language::Rust, &ProfileOverrides::default()));
    let stored = repo.outside().join("state/module-map.md");

    let first = load_or_create_summary(aci.workspace(), &stored).unwrap();
    assert_eq!(std::fs::read_to_string(&stored).unwrap(), first);
    // A later change in the repo does not rebuild it.
    repo.write("src/extra.rs", "pub struct Extra;\n");
    assert_eq!(
        load_or_create_summary(aci.workspace(), &stored).unwrap(),
        first
    );

    let inside = repo.repo().join(".aegis/module-map.md");
    assert!(matches!(
        load_or_create_summary(aci.workspace(), &inside),
        Err(AciError::InvalidInput(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_and_system_blocks_do_not_change_between_issues() {
    let repo = Scratch::fixture("rust");
    let rust = Profiles::builtin()
        .unwrap()
        .get(Language::Rust)
        .unwrap()
        .clone();
    let aci = repo.aci(profile(Language::Rust, &ProfileOverrides::default()));
    let map = load_or_create_summary(aci.workspace(), &repo.outside().join("map.md")).unwrap();
    let system = system_blocks(&RepoPrompt {
        language_conventions: rust.conventions(),
        repo_conventions: Some("- Keep changes inside one crate."),
        module_summary: Some(&map),
    });
    let spec = McpServerSpec {
        name: "aci".into(),
        program: env!("CARGO_BIN_EXE_aegis-aci").into(),
        args: vec![
            "--repo".into(),
            repo.repo().display().to_string(),
            "--language".into(),
            "rust".into(),
        ],
        cwd: None,
        env: vec![],
        tools: None,
    };

    let mut first_requests = Vec::new();
    for issue in [
        "Issue #101 (2026-10-20 09:14, session a1b2): a fourth item is accepted",
        "Issue #202 (2026-11-02 17:40, session c3d4): total overflows on large carts",
    ] {
        // A new server per issue, as each issue runs in its own session.
        let tools = McpTools::connect(std::slice::from_ref(&spec))
            .await
            .unwrap();
        let provider = Recorder::new();
        let session = Session::new(
            system.clone(),
            Message::user_text(issue),
            CacheTtl::FiveMinutes,
        );
        run_loop(
            &provider,
            &tools,
            &MaxModelCalls(5),
            &model(),
            session,
            &LoopConfig::default(),
        )
        .await;
        tools.shutdown().await;
        let requests = provider.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        first_requests.push(requests[0].clone());
    }
    let (a, b) = (&first_requests[0], &first_requests[1]);
    assert_eq!(a.tools, b.tools, "BP1 changed between issues");
    assert_eq!(a.system, b.system, "BP2 changed between issues");
    assert_ne!(a.messages, b.messages, "BP3 is where the issues differ");
    let bp2 = a.system.concat();
    for volatile in ["101", "202", "2026-", "session a1b2"] {
        assert!(!bp2.contains(volatile), "`{volatile}` in BP2");
    }
    assert!(bp2.contains("# Rust conventions") && bp2.contains("Keep changes inside one crate"));
}
