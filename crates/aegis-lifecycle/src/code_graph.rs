//! Codebase-Memory next to the ACI (design §3.2, #185): the job's tree is
//! indexed before the agent's first call, and again before a graph query
//! that follows an `edit` or `create_file`, so callers and callees are
//! those of the code as it is now. Indexing takes 2–6 s on typst and
//! scrapy.
//!
//! Every Codebase-Memory tool takes the indexed `project`, whose name comes
//! from the tree's path and so changes with each job. The agent never
//! sees it: the field is left out of the tools' schemas and filled in here,
//! which keeps the tool list, part of the cached prompt prefix (design
//! §4.4), the same for every job of a repo.

use std::collections::BTreeSet;
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
    /// `queries` are the Codebase-Memory tools given to the agent; `None`
    /// when the configuration has no Codebase-Memory.
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
        if graph.stale.swap(false, Ordering::SeqCst)
            && let Err(why) = index(self.tools, &graph.tree).await
        {
            graph.stale.store(true, Ordering::SeqCst);
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
            ToolOutcome {
                content: if fails {
                    "disk full".into()
                } else {
                    r#"{"project":"job-7","nodes":13}"#.into()
                },
                is_error: fails,
            }
        }
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
            let text = std::fs::read_to_string(&lib).unwrap() + input["text"].as_str().unwrap();
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
        let runtime = own_daemon_dir();
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

    /// A directory of the test's own for the server's daemon, which is
    /// otherwise one per user: a daemon already running for another cache
    /// (an eval, an editor) would refuse this test's server. Short, as it
    /// holds the daemon's socket, and private, as the server requires.
    fn own_daemon_dir() -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::path::PathBuf::from(format!("/tmp/aegis-cbm-{}", std::process::id()));
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
        graph.close().await;
        let calls = fake.calls.lock().unwrap().clone();
        assert_eq!(fake.names(), ["edit", "trace_path"]);
        assert_eq!(calls[1].1, json!({"project": "p"}));
    }
}
