//! #655: a mode with an embedding model recalls the memory nearest the
//! issue's meaning, through the server's recall: memories are embedded
//! before the recall, secrets are hidden from the embedding model, and a
//! failing model leaves recall lexical. Runs where the test server has
//! pgvector; the embedding model is a local stand-in.

use aegis_lifecycle::{IssueInput, MemorySection};
use aegis_provider::{EmbeddingConfig, LocalProtocol, ModelPolicy, ProviderConfig};
use aegis_secrets::Secrets;
use aegis_server::memory::{embedder_of, enrich_with_memory};
use aegis_store::testing::test_db;
use aegis_store::{MemoryKind, ModeName, NewRepo, NewRepoMemory, RepoId, Store};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// A stand-in model: refunds, in any language, point one way; anything
/// else another.
fn embed(text: &str) -> [f32; 3] {
    let refund = [
        "refund",
        "reimburs",
        "paying customers back",
        "返金",
        "払い戻",
    ];
    if refund.iter().any(|w| text.to_lowercase().contains(w)) {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    }
}

async fn model(status: u16) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(move |req: &Request| {
            if status != 200 {
                return ResponseTemplate::new(status);
            }
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let data: Vec<Value> = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, t)| json!({ "index": i, "embedding": embed(t.as_str().unwrap()) }))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({ "data": data }))
        })
        .mount(&server)
        .await;
    server
}

fn section(server: &MockServer) -> MemorySection {
    MemorySection {
        embedding: Some(EmbeddingConfig {
            provider: ProviderConfig::Local {
                base_url: format!("{}/v1", server.uri()),
                api_key_env: None,
                protocol: LocalProtocol::OpenAi,
            },
            model: "stand-in".into(),
            dimensions: 3,
            min_similarity: 0.5,
        }),
    }
}

async fn shop(s: &Store) -> RepoId {
    let id = s
        .create_repo(&NewRepo {
            slug: "shop".into(),
            url: "https://github.com/acme/shop".into(),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: ModeName::POC,
            max_concurrent_jobs: 1,
            config: json!({}),
        })
        .await
        .unwrap()
        .id;
    for (source_ref, title, summary) in [
        (
            "https://github.com/acme/shop/pull/7",
            "Stop paying customers back twice",
            "A retried request sent the same order's money again. token=sk-live-0123456789abcdef",
        ),
        (
            "https://github.com/acme/shop/pull/8",
            "Catalog card spacing",
            "Adjusted product card margins.",
        ),
    ] {
        s.upsert_repo_memory(&NewRepoMemory {
            repo_id: id,
            kind: MemoryKind::MergedPr,
            source_ref: source_ref.into(),
            title: title.into(),
            summary: summary.into(),
            paths: vec![],
            keywords: String::new(),
            job_id: None,
            scope: None,
        })
        .await
        .unwrap();
    }
    id
}

fn ticket() -> IssueInput {
    IssueInput {
        id: "shop-1".into(),
        title: "返金が二重に発生".into(),
        body: "同じ注文に二回".into(),
        url: "https://github.com/acme/shop/issues/1".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn the_nearest_memory_in_meaning_is_recalled() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    if !s.vector_search_ready(3).await {
        eprintln!("skipped: the test server has no pgvector");
        db.drop().await;
        return;
    }
    let repo = shop(s).await;
    let server = model(200).await;
    let embedder = embedder_of(&section(&server), ModelPolicy::Any, &Secrets::none())
        .await
        .unwrap();

    let mut lexical = ticket();
    enrich_with_memory(s, repo, &mut lexical, None).await;
    assert!(
        !lexical.first_message("x", true).contains("<repo-memory"),
        "no word in common"
    );

    let mut issue = ticket();
    enrich_with_memory(s, repo, &mut issue, Some(&embedder)).await;
    let bp3 = issue.first_message("x", true);
    assert!(bp3.contains("<repo-memory count=\"1\">"), "{bp3}");
    assert!(bp3.contains("Stop paying customers back twice"), "{bp3}");

    let sent: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect();
    assert_eq!(sent.len(), 2, "the two memories, then the issue");
    assert!(
        sent.iter().all(|b| !b.contains("sk-live-0123456789abcdef")),
        "{sent:?}"
    );

    // Embedded once: the next recall embeds only the issue.
    enrich_with_memory(s, repo, &mut ticket(), Some(&embedder)).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    db.drop().await;
}

#[tokio::test]
async fn a_failing_embedding_model_leaves_recall_lexical() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let repo = shop(s).await;
    let server = model(500).await;
    let embedder = embedder_of(&section(&server), ModelPolicy::Any, &Secrets::none())
        .await
        .unwrap();
    let mut issue = IssueInput {
        title: "Catalog card spacing is off".into(),
        ..ticket()
    };
    enrich_with_memory(s, repo, &mut issue, Some(&embedder)).await;
    let bp3 = issue.first_message("x", true);
    assert!(bp3.contains("Catalog card spacing"), "{bp3}");
    // Under `local_only`, a local endpoint still embeds.
    assert!(
        embedder_of(&section(&server), ModelPolicy::LocalOnly, &Secrets::none())
            .await
            .is_some(),
        "a local endpoint is allowed under local_only"
    );
    db.drop().await;
}
