//! "Xong khi" of #8: the orchestrator runs a real loop whose tools are the
//! `aegis-aci` binary, reached over MCP on stdio.

mod common;

use std::collections::VecDeque;
use std::sync::Mutex;

use aegis_orchestrator::{
    LoopConfig, LoopEnd, MaxModelCalls, McpServerSpec, McpTools, McpToolsError, ModelSpec, Session,
    ToolExecutor, run_loop,
};
use aegis_provider::{
    CacheTtl, ContentBlock, Message, ModelProvider, ProviderError, Request, Response, StopReason,
    Usage,
};
use common::Scratch;
use serde_json::{Value, json};

fn aci_spec(name: &str, repo: &Scratch, tools: Option<&[&str]>) -> McpServerSpec {
    McpServerSpec {
        name: name.to_owned(),
        program: env!("CARGO_BIN_EXE_aegis-aci").to_owned(),
        args: vec![
            "--repo".into(),
            repo.repo().display().to_string(),
            "--language".into(),
            "rust".into(),
        ],
        cwd: None,
        env: vec![],
        tools: tools.map(|t| t.iter().map(|s| (*s).to_owned()).collect()),
    }
}

struct ScriptedModel {
    script: Mutex<VecDeque<Response>>,
    requests: Mutex<Vec<Request>>,
}

impl ScriptedModel {
    /// One response per step: each asks for the given tool calls, then a
    /// final one ends the turn.
    fn new(steps: Vec<Vec<(&str, Value)>>) -> Self {
        let mut script: VecDeque<Response> = steps
            .into_iter()
            .enumerate()
            .map(|(i, calls)| {
                let content = calls
                    .into_iter()
                    .enumerate()
                    .map(|(j, (name, input))| ContentBlock::ToolUse {
                        id: format!("t{i}_{j}"),
                        name: name.to_owned(),
                        input,
                    })
                    .collect();
                response(content, StopReason::ToolUse)
            })
            .collect();
        script.push_back(response(
            vec![ContentBlock::text("done")],
            StopReason::EndTurn,
        ));
        Self {
            script: Mutex::new(script),
            requests: Mutex::new(vec![]),
        }
    }
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

impl ModelProvider for ScriptedModel {
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

/// `(content, is_error)` of every tool result the loop sent back, in order.
fn tool_results(session: &Session) -> Vec<(String, bool)> {
    session
        .messages()
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_list_is_sorted_with_schemas() {
    let repo = Scratch::fixture("rust");
    let tools = McpTools::connect(&[aci_spec("aci", &repo, None)])
        .await
        .unwrap();

    let names: Vec<&str> = tools
        .definitions()
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "create_file",
            "edit",
            "find_definition",
            "find_file",
            "find_references",
            "goto",
            "open",
            "scroll_down",
            "scroll_up",
            "search_symbol",
            "search_text"
        ]
    );
    let edit = &tools.definitions()[1];
    assert!(
        edit.description.contains("formatter"),
        "{}",
        edit.description
    );
    let required = edit.input_schema["required"].as_array().unwrap();
    for field in ["content", "file", "from", "to"] {
        assert!(required.contains(&json!(field)), "{required:?}");
    }
    tools.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn orchestrator_loop_runs_aci_tools_over_mcp() {
    let repo = Scratch::fixture("rust");
    let before = repo.read("src/lib.rs");
    let tools = McpTools::connect(&[aci_spec("aci", &repo, None)])
        .await
        .unwrap();
    let provider = ScriptedModel::new(vec![
        vec![("search_symbol", json!({"name": "Cart::add"}))],
        vec![
            ("open", json!({"file": "src/lib.rs", "line": 23})),
            ("scroll_down", Value::Null),
        ],
        vec![(
            "edit",
            json!({"file": "src/lib.rs", "from": 34, "to": 34, "content": "        self.items.iter().sum("}),
        )],
        vec![("open", json!({"file": "../etc/passwd"}))],
        vec![("goto", json!({"line": "ten"}))],
    ]);

    let outcome = run_loop(
        &provider,
        &tools,
        &MaxModelCalls(10),
        &model(),
        Session::new(
            vec!["sys".into()],
            Message::user_text("fix it"),
            CacheTtl::FiveMinutes,
        ),
        &LoopConfig::default(),
    )
    .await;
    tools.shutdown().await;

    assert!(
        matches!(outcome.end, LoopEnd::Finished { .. }),
        "{:?}",
        outcome.end
    );
    let results = tool_results(&outcome.session);
    assert_eq!(results.len(), 6, "{results:#?}");
    assert_eq!(
        results[0],
        (
            "1 matches for \"Cart::add\":\nsrc/lib.rs:23-29 method Cart::add: \
             pub fn add(&mut self, price: u32) -> Result<(), CartError>"
                .to_owned(),
            false
        )
    );
    assert!(
        results[1]
            .0
            .starts_with("[File: src/lib.rs (61 lines total)]\n1://!")
    );
    // A short file has nothing below: scrolling keeps the same window.
    assert_eq!(results[2], results[1]);
    assert!(results[3].1, "a refused edit is a failed call");
    assert!(
        results[3]
            .0
            .starts_with("Edit rejected, `src/lib.rs` is unchanged")
    );
    assert_eq!(
        results[4],
        (
            "`../etc/passwd` is outside the workspace; use a path relative to the repo root"
                .to_owned(),
            true
        )
    );
    assert!(results[5].1, "bad arguments are an error: {}", results[5].0);
    assert_eq!(repo.read("src/lib.rs"), before);
    // Every model call saw the same tool list.
    let requests = provider.requests.lock().unwrap();
    assert!(requests.iter().all(|r| r.tools == requests[0].tools));
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_list_narrows_and_unknown_or_duplicate_tools_fail() {
    let repo = Scratch::fixture("rust");
    let tools = McpTools::connect(&[aci_spec("aci", &repo, Some(&["open", "search_text"]))])
        .await
        .unwrap();
    let names: Vec<&str> = tools
        .definitions()
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names, ["open", "search_text"]);
    let refused = tools.call("edit", &json!({})).await;
    assert!(refused.is_error);
    assert_eq!(refused.content, "unknown tool `edit`");
    tools.shutdown().await;

    let missing = McpTools::connect(&[aci_spec("aci", &repo, Some(&["open", "rm"]))]).await;
    assert!(matches!(missing, Err(McpToolsError::MissingTool { tool, .. }) if tool == "rm"));

    let twice = McpTools::connect(&[
        aci_spec("first", &repo, Some(&["open"])),
        aci_spec("second", &repo, Some(&["open"])),
    ])
    .await;
    assert!(matches!(
        twice,
        Err(McpToolsError::DuplicateTool { first, second, .. }) if first == "first" && second == "second"
    ));

    let broken = McpTools::connect(&[McpServerSpec {
        args: vec!["--language".into(), "cobol".into()],
        ..aci_spec("broken", &repo, None)
    }])
    .await;
    assert!(
        matches!(broken, Err(McpToolsError::Start { .. })),
        "{broken:?}"
    );
}

/// Codebase-Memory next to the ACI server, as the agent will use them.
/// Runs only when `AEGIS_CBM_BIN` points at a `codebase-memory-mcp` binary;
/// its index lives in a throwaway `HOME`.
#[tokio::test(flavor = "multi_thread")]
async fn aci_and_codebase_memory_share_one_tool_list() {
    let Ok(cbm) = std::env::var("AEGIS_CBM_BIN") else {
        eprintln!("skipping: AEGIS_CBM_BIN is not set");
        return;
    };
    let repo = Scratch::fixture("rust");
    let home = repo.outside();
    std::fs::create_dir_all(&home).unwrap();
    let cbm_spec = McpServerSpec {
        name: "codebase-memory".into(),
        program: cbm,
        args: vec![],
        cwd: None,
        env: vec![("HOME".into(), home.display().to_string())],
        tools: Some(vec![
            "index_repository".into(),
            "search_graph".into(),
            "trace_path".into(),
        ]),
    };
    let tools = McpTools::connect(&[aci_spec("aci", &repo, Some(&["open"])), cbm_spec])
        .await
        .unwrap();
    let names: Vec<&str> = tools
        .definitions()
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["index_repository", "open", "search_graph", "trace_path"]
    );

    let indexed = tools
        .call(
            "index_repository",
            &json!({"repo_path": repo.repo().display().to_string(), "name": "fixture"}),
        )
        .await;
    assert!(!indexed.is_error, "{}", indexed.content);
    let found = tools
        .call(
            "search_graph",
            &json!({"project": "fixture", "name_pattern": "add"}),
        )
        .await;
    assert!(
        !found.is_error && found.content.contains("add"),
        "{}",
        found.content
    );
    tools.shutdown().await;
}

/// The tool list is part of the cached prompt prefix (design §4.4): any
/// change to a name, description or schema shows up here first. Run with
/// `AEGIS_UPDATE_SNAPSHOTS=1` to accept a deliberate change.
#[tokio::test(flavor = "multi_thread")]
async fn tool_definitions_match_the_snapshot() {
    let repo = Scratch::fixture("rust");
    let tools = McpTools::connect(&[aci_spec("aci", &repo, None)])
        .await
        .unwrap();
    let json: Vec<Value> = tools
        .definitions()
        .iter()
        .map(|d| json!({"name": d.name, "description": d.description, "input_schema": d.input_schema}))
        .collect();
    tools.shutdown().await;
    let actual = serde_json::to_string_pretty(&json).unwrap() + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/tools.json");
    if std::env::var("AEGIS_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        actual == expected,
        "tool definitions changed; check them and rerun with AEGIS_UPDATE_SNAPSHOTS=1:\n{actual}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn secret_values_never_reach_the_model_and_cannot_be_written_back() {
    let repo = Scratch::fixture("rust");
    // Built at run time so the source holds no secret-shaped string.
    let token = format!("ghp_{}", "Z".repeat(36));
    repo.write(
        "src/config.rs",
        &format!("// Deploy token, rotated yearly.\npub const TOKEN: &str = \"{token}\";\n"),
    );
    let tools = McpTools::connect(&[aci_spec("aci", &repo, None)])
        .await
        .unwrap();

    let opened = tools.call("open", &json!({"file": "src/config.rs"})).await;
    assert!(!opened.is_error, "{}", opened.content);
    assert!(!opened.content.contains(&token));
    assert!(
        opened
            .content
            .contains("1:// Deploy token, rotated yearly."),
        "comments stay"
    );
    assert!(
        opened
            .content
            .contains("2:pub const TOKEN: &str = \"[REDACTED:github-token]\";")
    );
    assert!(
        opened
            .content
            .contains("[aegis: 1 secret value(s) hidden (github-token).")
    );

    let found = tools.call("search_text", &json!({"text": "TOKEN"})).await;
    assert!(!found.content.contains(&token), "{}", found.content);

    let written_back = tools
        .call(
            "edit",
            &json!({"file": "src/config.rs", "from": 2, "to": 2,
                    "content": "pub const TOKEN: &str = \"[REDACTED:github-token]\";"}),
        )
        .await;
    assert!(written_back.is_error);
    assert!(
        written_back.content.contains("hidden secret value"),
        "{}",
        written_back.content
    );
    assert!(
        repo.read("src/config.rs").contains(&token),
        "the file keeps the real value"
    );
    tools.shutdown().await;
}
