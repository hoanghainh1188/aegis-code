//! Codebase-Memory next to the ACI (design §3.2, #185): the job's tree is
//! indexed before the agent's first call, and again before a graph query
//! that follows an `edit` or `create_file`, so callers and callees are
//! those of the code as it is now. Indexing takes 2–6 s on typst and
//! scrapy.
//!
//! The agent made no graph query in the 32 runs of #185, so the platform
//! asks the graph itself (#203): before the first gate run, [`CodeGraph::impact`]
//! lists the code outside the patch's files that calls, or is called by,
//! the functions the patch changed, for the agent to check.
//!
//! Every Codebase-Memory tool takes the indexed `project`, whose name comes
//! from the tree's path and so changes with each job. The agent never
//! sees it: the field is left out of the tools' schemas and filled in here,
//! which keeps the tool list, part of the cached prompt prefix (design
//! §4.4), the same for every job of a repo.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use aegis_orchestrator::{McpTools, ToolExecutor, ToolOutcome};
use aegis_provider::ToolDefinition;
use serde_json::{Value, json};

use crate::LifecycleError;

/// The server's name among the job's MCP servers.
pub(crate) const SERVER: &str = "codebase-memory";

/// ACI tools after which the index is out of date.
const WRITES: [&str; 2] = ["edit", "create_file"];

/// `fast` leaves out docs and the semantic layer, which needs a model
/// download (#3 found the docs' edges noisy).
const INDEX_MODE: &str = "fast";

/// Files of each direction an impact note names at most, and functions
/// of each file: enough for the misses of #185 (2–5 files), short enough
/// not to send the agent reading half the repo.
const IMPACT_FILES: usize = 8;
const IMPACT_NAMES: usize = 4;

/// The calls the platform makes itself, on a server's tools the agent may
/// not have.
pub(crate) trait ServerCalls: ToolExecutor {
    fn call_server(
        &self,
        server: &str,
        name: &str,
        input: &Value,
    ) -> impl Future<Output = ToolOutcome> + Send;
}

impl ServerCalls for McpTools {
    fn call_server(
        &self,
        server: &str,
        name: &str,
        input: &Value,
    ) -> impl Future<Output = ToolOutcome> + Send {
        McpTools::call_server(self, server, name, input)
    }
}

/// The job's tools, with the graph's kept up to date.
pub(crate) struct CodeGraph<'a, T> {
    tools: &'a T,
    definitions: Vec<ToolDefinition>,
    graph: Option<Graph>,
}

struct Graph {
    tree: String,
    project: String,
    /// The Codebase-Memory tools the agent has.
    queries: BTreeSet<String>,
    stale: AtomicBool,
}

impl<'a, T: ServerCalls + Sync> CodeGraph<'a, T> {
    /// `queries` are the Codebase-Memory tools given to the agent, maybe
    /// none; `None` when the configuration has no Codebase-Memory, and the
    /// tree is then not indexed. Tool calls run one after another
    /// (`run_loop`), so a query never overlaps an edit or another index.
    pub(crate) async fn open(
        tools: &'a T,
        queries: Option<&[String]>,
        tree: &Path,
    ) -> Result<Self, LifecycleError> {
        let Some(queries) = queries else {
            return Ok(Self {
                tools,
                definitions: tools.definitions().to_vec(),
                graph: None,
            });
        };
        let tree = tree.display().to_string();
        let project = index(tools, &tree)
            .await
            .map_err(LifecycleError::CodeGraph)?;
        let queries: BTreeSet<String> = queries.iter().cloned().collect();
        let definitions = tools
            .definitions()
            .iter()
            .map(|d| {
                if queries.contains(&d.name) {
                    without_project(d)
                } else {
                    d.clone()
                }
            })
            .collect();
        Ok(Self {
            tools,
            definitions,
            graph: Some(Graph {
                tree,
                project,
                queries,
                stale: AtomicBool::new(false),
            }),
        })
    }

    /// The code outside the files changed since `base` that the graph links
    /// to the functions changed (#203); `None` without Codebase-Memory.
    pub(crate) async fn impact(&self, base: &str) -> Option<Result<Impact, String>> {
        let graph = self.graph.as_ref()?;
        Some(self.impact_of(graph, base).await)
    }

    async fn impact_of(&self, graph: &Graph, base: &str) -> Result<Impact, String> {
        refresh(self.tools, graph).await?;
        let mut impact = Impact::default();
        for (direction, side) in [
            ("inbound", &mut impact.callers),
            ("outbound", &mut impact.callees),
        ] {
            let input = json!({
                "project": graph.project,
                "direction": direction,
                "depth": 1,
                "base_branch": base,
                "seed_filter": "product",
                "limit": 500,
                "format": "json",
            });
            let outcome = self
                .tools
                .call_server(SERVER, "detect_changes", &input)
                .await;
            if outcome.is_error {
                return Err(outcome.content);
            }
            *side = linked(&graph.project, &outcome.content)?;
        }
        Ok(impact)
    }

    /// Drops the job's index. Best effort, as removing the job's worktree:
    /// a left-over index only takes room under the repo's index directory.
    pub(crate) async fn close(self) {
        if let Some(graph) = &self.graph {
            let input = json!({ "project": graph.project });
            let _ = self
                .tools
                .call_server(SERVER, "delete_project", &input)
                .await;
        }
    }
}

impl<T: ServerCalls + Sync> ToolExecutor for CodeGraph<'_, T> {
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    async fn call(&self, name: &str, input: &Value) -> ToolOutcome {
        let Some(graph) = self.graph.as_ref() else {
            return self.tools.call(name, input).await;
        };
        if !graph.queries.contains(name) {
            let outcome = self.tools.call(name, input).await;
            if WRITES.contains(&name) && !outcome.is_error {
                graph.stale.store(true, Ordering::SeqCst);
            }
            return outcome;
        }
        if let Err(why) = refresh(self.tools, graph).await {
            return ToolOutcome {
                content: format!("the code graph could not be brought up to date: {why}"),
                is_error: true,
            };
        }
        let mut input = match input {
            Value::Object(map) => map.clone(),
            Value::Null => serde_json::Map::new(),
            _ => {
                return ToolOutcome {
                    content: format!("the input of `{name}` must be a JSON object"),
                    is_error: true,
                };
            }
        };
        input.insert("project".into(), Value::String(graph.project.clone()));
        self.tools.call(name, &Value::Object(input)).await
    }
}

/// Indexes the tree again when an edit came after the last index.
async fn refresh<T: ServerCalls + Sync>(tools: &T, graph: &Graph) -> Result<(), String> {
    if graph.stale.swap(false, Ordering::SeqCst)
        && let Err(why) = index(tools, &graph.tree).await
    {
        graph.stale.store(true, Ordering::SeqCst);
        return Err(why);
    }
    Ok(())
}

/// Code the graph links to a patch, by file: the callers of the functions
/// it changed, and the functions they call. Neither holds the patch's own
/// files or tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Impact {
    pub(crate) callers: BTreeMap<String, BTreeSet<String>>,
    pub(crate) callees: BTreeMap<String, BTreeSet<String>>,
}

impl Impact {
    /// The files named, callers' and callees' together.
    pub(crate) fn files(&self) -> Vec<String> {
        let files: BTreeSet<&String> = self.callers.keys().chain(self.callees.keys()).collect();
        files.into_iter().cloned().collect()
    }

    /// What the agent reads before the gate; `None` when the graph links
    /// nothing outside the patch.
    pub(crate) fn note(&self) -> Option<String> {
        if self.callers.is_empty() && self.callees.is_empty() {
            return None;
        }
        let mut text = String::from(
            "Before the checks run: the code graph links the functions you changed to code \
             in files your patch does not touch.\n",
        );
        for (title, side) in [
            ("Called from", &self.callers),
            ("Calls into", &self.callees),
        ] {
            if side.is_empty() {
                continue;
            }
            text.push_str(&format!("\n{title}:\n"));
            text.push_str(&listing(side));
        }
        text.push_str(
            "\nCheck whether the fix needs a change there too: the same fault in a place \
             that does the same job, or a caller that relies on what you changed. The graph \
             matches calls by name, so read the code before you change it. Then reply with \
             the final JSON answer again, the same one if nothing else needs to change.",
        );
        Some(text)
    }
}

/// One line per file, the files with the most functions first.
fn listing(side: &BTreeMap<String, BTreeSet<String>>) -> String {
    let mut files: Vec<_> = side.iter().collect();
    files.sort_by_key(|(file, names)| (std::cmp::Reverse(names.len()), *file));
    let mut text = String::new();
    for (file, names) in files.iter().take(IMPACT_FILES) {
        let shown: Vec<String> = names
            .iter()
            .take(IMPACT_NAMES)
            .map(|n| format!("`{n}`"))
            .collect();
        let more = names.len().saturating_sub(IMPACT_NAMES);
        let more = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        text.push_str(&format!("- {file}: {}{more}\n", shown.join(", ")));
    }
    let left = files.len().saturating_sub(IMPACT_FILES);
    if left > 0 {
        text.push_str(&format!("- and {left} more files\n"));
    }
    text
}

/// The functions of `detect_changes`' JSON answer, by file, without the
/// changed files (the agent's new files among them), tests, nodes that
/// stand for a whole file, and the language's own (`<python-builtins>`).
fn linked(project: &str, answer: &str) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let answer: Value = serde_json::from_str(answer)
        .map_err(|e| format!("the graph's answer is not JSON ({e}): {answer}"))?;
    let changed: BTreeSet<&str> = answer["changed_files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut linked: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for node in answer["impacted"].as_array().into_iter().flatten() {
        let (Some(qn), Some(file)) = (node["qn"].as_str(), node["file"].as_str()) else {
            continue;
        };
        if node["label"] == "File"
            || file.starts_with('<')
            || changed.contains(file)
            || is_test(file)
        {
            continue;
        }
        linked
            .entry(file.to_owned())
            .or_default()
            .insert(short_name(project, file, qn));
    }
    Ok(linked)
}

/// `qn` without the project and the file's module path:
/// `Scheduler.next_request` for `<project>.scrapy.core.scheduler.Scheduler.next_request`.
fn short_name(project: &str, file: &str, qn: &str) -> String {
    let module = file
        .rsplit_once('.')
        .map_or(file, |(stem, _)| stem)
        .replace('/', ".");
    qn.strip_prefix(&format!("{project}.{module}."))
        .or_else(|| qn.rsplit('.').next())
        .unwrap_or(qn)
        .to_owned()
}

/// A test file by its path, in any of the profiles' languages.
fn is_test(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    let stem = name.split('.').next().unwrap_or(name);
    file.split('/').any(|dir| dir == "tests" || dir == "test")
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
}

/// Indexes `tree` and returns the project's name, which the server picks.
async fn index<T: ServerCalls + Sync>(tools: &T, tree: &str) -> Result<String, String> {
    let input = json!({ "repo_path": tree, "mode": INDEX_MODE });
    let outcome = tools.call_server(SERVER, "index_repository", &input).await;
    if outcome.is_error {
        return Err(outcome.content);
    }
    serde_json::from_str::<Value>(&outcome.content)
        .ok()
        .and_then(|v| v.get("project")?.as_str().map(str::to_owned))
        .ok_or_else(|| format!("no project in the index's answer: {}", outcome.content))
}

/// `d` with no `project` among its properties and required fields.
fn without_project(d: &ToolDefinition) -> ToolDefinition {
    let mut schema = d.input_schema.clone();
    if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        props.remove("project");
    }
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|r| r != "project");
    }
    ToolDefinition {
        input_schema: schema,
        ..d.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// One ACI tool and one graph tool; records every call.
    struct Fake {
        definitions: Vec<ToolDefinition>,
        calls: Mutex<Vec<(String, Value)>>,
        index_fails: AtomicBool,
    }

    impl Fake {
        fn new() -> Self {
            let tool = |name: &str, schema: Value| ToolDefinition {
                name: name.into(),
                description: String::new(),
                input_schema: schema,
            };
            Self {
                definitions: vec![
                    tool("edit", json!({"type": "object"})),
                    tool(
                        "trace_path",
                        json!({
                            "type": "object",
                            "properties": {"function_name": {}, "project": {}},
                            "required": ["function_name", "project"],
                        }),
                    ),
                ],
                calls: Mutex::new(Vec::new()),
                index_fails: AtomicBool::new(false),
            }
        }

        fn names(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(n, _)| n.clone())
                .collect()
        }
    }

    impl ToolExecutor for Fake {
        fn definitions(&self) -> &[ToolDefinition] {
            &self.definitions
        }

        async fn call(&self, name: &str, input: &Value) -> ToolOutcome {
            self.calls
                .lock()
                .unwrap()
                .push((name.into(), input.clone()));
            ToolOutcome {
                content: "ok".into(),
                is_error: input.get("fail").is_some(),
            }
        }
    }

    impl ServerCalls for Fake {
        async fn call_server(&self, server: &str, name: &str, input: &Value) -> ToolOutcome {
            assert_eq!(server, SERVER);
            self.calls
                .lock()
                .unwrap()
                .push((name.into(), input.clone()));
            let fails = name == "index_repository" && self.index_fails.load(Ordering::SeqCst);
            let content = match (name, input["direction"].as_str()) {
                _ if fails => "disk full".into(),
                ("detect_changes", Some("inbound")) => changes(&[
                    ("Scheduler.enqueue", "scrapy/core/scheduler.py"),
                    ("ExecutionEngine.crawl", "scrapy/core/engine.py"),
                    ("ExecutionEngine.schedule", "scrapy/core/engine.py"),
                    ("test_scheduler.test_order", "tests/test_scheduler.py"),
                ]),
                ("detect_changes", _) => changes(&[
                    ("pqueues.__file__", "scrapy/pqueues.py"),
                    ("len", "<python-builtins>"),
                    ("ScrapyPriorityQueue.pop", "scrapy/pqueues.py"),
                ]),
                _ => r#"{"project":"job-7","nodes":13}"#.into(),
            };
            ToolOutcome {
                content,
                is_error: fails,
            }
        }
    }

    /// A `detect_changes` answer for a patch to `scrapy/core/scheduler.py`,
    /// as the server gives it: qualified names start with the project.
    fn changes(nodes: &[(&str, &str)]) -> String {
        let impacted: Vec<Value> = nodes
            .iter()
            .map(|(name, file)| {
                let module = file.trim_end_matches(".py").replace('/', ".");
                let label = if name.ends_with("__file__") {
                    "File"
                } else {
                    "Method"
                };
                json!({"qn": format!("job-7.{module}.{name}"), "label": label, "file": file, "hop": 1})
            })
            .collect();
        json!({"changed_files": ["scrapy/core/scheduler.py"], "impacted": impacted}).to_string()
    }

    fn queries() -> Vec<String> {
        vec!["trace_path".into()]
    }

    #[tokio::test]
    async fn the_agent_never_sees_or_gives_the_project() {
        let fake = Fake::new();
        let queries = queries();
        let graph = CodeGraph::open(&fake, Some(&queries), Path::new("/jobs/7/repo"))
            .await
            .unwrap();
        let trace = &graph.definitions()[1].input_schema;
        assert_eq!(trace["required"], json!(["function_name"]));
        assert!(trace["properties"].get("project").is_none());
        assert_eq!(graph.definitions()[0], fake.definitions[0]);

        graph
            .call(
                "trace_path",
                &json!({"function_name": "add", "project": "x"}),
            )
            .await;
        let calls = fake.calls.lock().unwrap().clone();
        assert_eq!(
            calls[0],
            (
                "index_repository".into(),
                json!({"repo_path": "/jobs/7/repo", "mode": "fast"})
            )
        );
        assert_eq!(
            calls[1].1,
            json!({"function_name": "add", "project": "job-7"})
        );
    }

    #[tokio::test]
    async fn a_query_after_an_edit_indexes_again_once() {
        let fake = Fake::new();
        let queries = queries();
        let graph = CodeGraph::open(&fake, Some(&queries), Path::new("/t"))
            .await
            .unwrap();
        graph.call("edit", &json!({"fail": true})).await;
        graph.call("trace_path", &json!({})).await;
        graph.call("edit", &json!({})).await;
        graph.call("edit", &json!({})).await;
        graph.call("trace_path", &json!({})).await;
        graph.call("trace_path", &json!({})).await;
        graph.close().await;
        assert_eq!(
            fake.names(),
            [
                "index_repository",
                "edit",
                "trace_path",
                "edit",
                "edit",
                "index_repository",
                "trace_path",
                "trace_path",
                "delete_project",
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_reindex_is_the_query_s_error_and_is_tried_again() {
        let fake = Fake::new();
        let queries = queries();
        let graph = CodeGraph::open(&fake, Some(&queries), Path::new("/t"))
            .await
            .unwrap();
        graph.call("edit", &json!({})).await;
        fake.index_fails.store(true, Ordering::SeqCst);
        let failed = graph.call("trace_path", &json!({})).await;
        assert!(
            failed.is_error && failed.content.contains("disk full"),
            "{failed:?}"
        );
        fake.index_fails.store(false, Ordering::SeqCst);
        assert!(!graph.call("trace_path", &json!({})).await.is_error);
        assert_eq!(
            fake.names(),
            [
                "index_repository",
                "edit",
                "index_repository",
                "index_repository",
                "trace_path",
            ]
        );
    }

    #[tokio::test]
    async fn a_first_index_that_fails_stops_the_job() {
        let fake = Fake::new();
        fake.index_fails.store(true, Ordering::SeqCst);
        let queries = queries();
        let opened = CodeGraph::open(&fake, Some(&queries), Path::new("/t")).await;
        assert!(
            matches!(&opened, Err(LifecycleError::CodeGraph(m)) if m.contains("disk full")),
            "{:?}",
            opened.err()
        );
    }

    /// The real server, and an `edit` that appends its `text` to the tree's
    /// `src/lib.rs`.
    struct WithEdit {
        tools: McpTools,
        tree: std::path::PathBuf,
    }

    impl ToolExecutor for WithEdit {
        fn definitions(&self) -> &[ToolDefinition] {
            self.tools.definitions()
        }

        async fn call(&self, name: &str, input: &Value) -> ToolOutcome {
            if name != "edit" {
                return self.tools.call(name, input).await;
            }
            let lib = self.tree.join("src/lib.rs");
            let text = match input["content"].as_str() {
                Some(content) => content.to_owned(),
                None => std::fs::read_to_string(&lib).unwrap() + input["text"].as_str().unwrap(),
            };
            std::fs::write(lib, text).unwrap();
            ToolOutcome {
                content: "ok".into(),
                is_error: false,
            }
        }
    }

    impl ServerCalls for WithEdit {
        async fn call_server(&self, server: &str, name: &str, input: &Value) -> ToolOutcome {
            self.tools.call_server(server, name, input).await
        }
    }

    /// Runs only when `AEGIS_CBM_BIN` points at a `codebase-memory-mcp`
    /// binary, as in aegis-aci's `mcp_loop` test.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_real_graph_follows_the_agent_s_edits() {
        let Ok(program) = std::env::var("AEGIS_CBM_BIN") else {
            eprintln!("skipping: AEGIS_CBM_BIN is not set");
            return;
        };
        let scratch = std::env::temp_dir().join(format!("aegis-cbm-{}", std::process::id()));
        let tree = scratch.join("repo");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(
            tree.join("Cargo.toml"),
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            tree.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\
             pub fn twice(a: i32) -> i32 { add(a, a) }\n",
        )
        .unwrap();
        let home = scratch.join("index");
        std::fs::create_dir_all(&home).unwrap();
        let runtime = own_daemon_dir("edits");
        let spec = aegis_orchestrator::McpServerSpec {
            name: SERVER.into(),
            program,
            args: vec!["--ui=false".into()],
            cwd: Some(tree.clone()),
            env: vec![
                ("HOME".into(), home.display().to_string()),
                ("CBM_RUNTIME_DIR".into(), runtime.display().to_string()),
            ],
            tools: Some(queries()),
        };
        let tools = WithEdit {
            tools: McpTools::connect(&[spec]).await.unwrap(),
            tree: tree.clone(),
        };
        let queries = queries();
        let graph = CodeGraph::open(&tools, Some(&queries), &tree)
            .await
            .unwrap();
        let inbound = json!({"function_name": "add", "direction": "inbound"});
        let before = graph.call("trace_path", &inbound).await;
        assert!(
            !before.is_error && before.content.contains("twice"),
            "{}",
            before.content
        );
        assert!(!before.content.contains("thrice"), "{}", before.content);

        graph
            .call(
                "edit",
                &json!({"text": "pub fn thrice(a: i32) -> i32 { add(twice(a), a) }\n"}),
            )
            .await;
        let after = graph.call("trace_path", &inbound).await;
        assert!(after.content.contains("thrice"), "{}", after.content);

        graph.close().await;
        let projects = tools
            .tools
            .call_server(SERVER, "list_projects", &json!({}))
            .await;
        assert!(
            projects.content.contains("projects: 0"),
            "{}",
            projects.content
        );
        tools.tools.shutdown().await;
        let _ = std::fs::remove_dir_all(&scratch);
        let _ = std::fs::remove_dir_all(&runtime);
    }

    /// As the test above, on a git checkout: the graph names a caller of
    /// a function the patch changed, in a file the patch does not touch.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_real_graph_names_the_callers_outside_the_patch() {
        let Ok(program) = std::env::var("AEGIS_CBM_BIN") else {
            eprintln!("skipping: AEGIS_CBM_BIN is not set");
            return;
        };
        let scratch = std::env::temp_dir().join(format!("aegis-cbm-impact-{}", std::process::id()));
        let tree = scratch.join("repo");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(
            tree.join("Cargo.toml"),
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let lib = "mod other;\n\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        std::fs::write(tree.join("src/lib.rs"), lib).unwrap();
        std::fs::write(
            tree.join("src/other.rs"),
            "pub fn twice(a: i32) -> i32 {\n    crate::add(a, a)\n}\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&tree)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        let home = scratch.join("index");
        std::fs::create_dir_all(&home).unwrap();
        let runtime = own_daemon_dir("impact");
        let spec = aegis_orchestrator::McpServerSpec {
            name: SERVER.into(),
            program,
            args: vec!["--ui=false".into()],
            cwd: Some(tree.clone()),
            env: vec![
                ("HOME".into(), home.display().to_string()),
                ("CBM_RUNTIME_DIR".into(), runtime.display().to_string()),
            ],
            tools: Some(vec![]),
        };
        let tools = WithEdit {
            tools: McpTools::connect(&[spec]).await.unwrap(),
            tree: tree.clone(),
        };
        let none: &[String] = &[];
        let graph = CodeGraph::open(&tools, Some(none), &tree).await.unwrap();
        let patched = lib.replace("a + b", "a.saturating_add(b)");
        graph.call("edit", &json!({"content": patched})).await;
        let impact = graph.impact(&base).await.unwrap().unwrap();
        assert!(
            impact
                .callers
                .get("src/other.rs")
                .is_some_and(|n| n.contains("twice")),
            "{impact:?}"
        );
        assert!(!impact.files().contains(&"src/lib.rs".to_owned()));

        graph.close().await;
        tools.tools.shutdown().await;
        let _ = std::fs::remove_dir_all(&scratch);
        let _ = std::fs::remove_dir_all(&runtime);
    }

    /// A directory of the test's own for the server's daemon, which is
    /// otherwise one per user: a daemon already running for another cache
    /// (an eval, an editor) would refuse this test's server. Short, as it
    /// holds the daemon's socket, and private, as the server requires; one
    /// per test, as the tests run at once.
    fn own_daemon_dir(test: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::path::PathBuf::from(format!("/tmp/aegis-cbm-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[tokio::test]
    async fn without_codebase_memory_the_tools_pass_through() {
        let fake = Fake::new();
        let graph = CodeGraph::open(&fake, None, Path::new("/t")).await.unwrap();
        assert_eq!(graph.definitions(), &fake.definitions[..]);
        graph.call("edit", &json!({})).await;
        graph.call("trace_path", &json!({"project": "p"})).await;
        assert!(graph.impact("base").await.is_none());
        graph.close().await;
        let calls = fake.calls.lock().unwrap().clone();
        assert_eq!(fake.names(), ["edit", "trace_path"]);
        assert_eq!(calls[1].1, json!({"project": "p"}));
    }

    #[tokio::test]
    async fn with_no_tools_for_the_agent_the_tree_is_still_indexed() {
        let fake = Fake::new();
        let none: &[String] = &[];
        let graph = CodeGraph::open(&fake, Some(none), Path::new("/t"))
            .await
            .unwrap();
        assert_eq!(graph.definitions(), &fake.definitions[..]);
        graph.call("trace_path", &json!({"project": "p"})).await;
        graph.close().await;
        assert_eq!(
            fake.names(),
            ["index_repository", "trace_path", "delete_project"]
        );
    }

    #[tokio::test]
    async fn the_impact_names_linked_code_outside_the_patch() {
        let fake = Fake::new();
        let none: &[String] = &[];
        let graph = CodeGraph::open(&fake, Some(none), Path::new("/t"))
            .await
            .unwrap();
        graph.call("edit", &json!({})).await;
        let impact = graph.impact("abc123").await.unwrap().unwrap();
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(
            impact.callers,
            BTreeMap::from([(
                "scrapy/core/engine.py".to_owned(),
                names(&["ExecutionEngine.crawl", "ExecutionEngine.schedule"])
            )])
        );
        assert_eq!(
            impact.callees,
            BTreeMap::from([(
                "scrapy/pqueues.py".to_owned(),
                names(&["ScrapyPriorityQueue.pop"])
            )])
        );
        assert_eq!(
            impact.files(),
            ["scrapy/core/engine.py", "scrapy/pqueues.py"]
        );
        let note = impact.note().unwrap();
        assert!(
            note.contains(
                "Called from:\n- scrapy/core/engine.py: `ExecutionEngine.crawl`, \
                 `ExecutionEngine.schedule`\n"
            ),
            "{note}"
        );
        assert!(
            note.contains("Calls into:\n- scrapy/pqueues.py: `ScrapyPriorityQueue.pop`\n"),
            "{note}"
        );
        // The edit made the index stale: indexed again before the query.
        let calls = fake.calls.lock().unwrap().clone();
        let asked: Vec<_> = calls.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            asked,
            [
                "index_repository",
                "edit",
                "index_repository",
                "detect_changes",
                "detect_changes"
            ]
        );
        assert_eq!(calls[3].1["base_branch"], "abc123");
        assert_eq!(calls[3].1["project"], "job-7");
    }

    #[tokio::test]
    async fn a_failed_impact_query_is_an_error() {
        let fake = Fake::new();
        let none: &[String] = &[];
        let graph = CodeGraph::open(&fake, Some(none), Path::new("/t"))
            .await
            .unwrap();
        graph.call("edit", &json!({})).await;
        fake.index_fails.store(true, Ordering::SeqCst);
        let failed = graph.impact("abc123").await.unwrap();
        assert_eq!(failed, Err("disk full".to_owned()));
    }

    #[test]
    fn a_patch_with_nothing_linked_needs_no_note() {
        assert_eq!(Impact::default().note(), None);
    }

    #[test]
    fn the_note_keeps_to_the_files_with_most_functions() {
        let side: BTreeMap<String, BTreeSet<String>> = (0..10)
            .map(|i| {
                let names = (0..=i).map(|n| format!("f{n}")).collect();
                (format!("src/m{i}.rs"), names)
            })
            .collect();
        let text = listing(&side);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), IMPACT_FILES + 1, "{text}");
        assert_eq!(lines[0], "- src/m9.rs: `f0`, `f1`, `f2`, `f3` and 6 more");
        assert_eq!(lines[IMPACT_FILES], "- and 2 more files");
    }

    #[test]
    fn short_names_and_tests() {
        assert_eq!(
            short_name(
                "p",
                "crates/typst-pdf/src/tags/mod.rs",
                "p.crates.typst-pdf.src.tags.mod.handle_start"
            ),
            "handle_start"
        );
        assert_eq!(short_name("p", "a/b.py", "q.other.Thing.run"), "run");
        for test in [
            "tests/test_engine.py",
            "tests/src/report/html.rs",
            "crates/x/src/foo_test.go",
            "scrapy/test_x.py",
        ] {
            assert!(is_test(test), "{test}");
        }
        for source in [
            "scrapy/core/engine.py",
            "crates/typst-pdf/src/tests_util.rs",
        ] {
            assert!(!is_test(source), "{source}");
        }
    }
}
