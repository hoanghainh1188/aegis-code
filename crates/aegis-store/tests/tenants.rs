//! #525: tenants (contracts). Each repo is in one; a tenant's monthly
//! budget caps all its repos together, its job cap bounds what runs at
//! once over them, and the claim shares workers between tenants. Tenant
//! admins administer only their repos. Everything is audited.

use std::time::Duration;

use aegis_ledger::{IssueId, LedgerEvent, LedgerStore, Tier, TokenCounts};
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, AuditFilter, Job, JobSource, ModeName, NewJob, NewRepo, Page, RepoId, Store, StoreError,
    TenantId, UserKind,
};

const LEASE: Duration = Duration::from_secs(60);
const ANY: u32 = 10;

fn new_repo(slug: &str) -> NewRepo {
    NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/example/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 10,
        config: serde_json::json!({}),
    }
}

async fn repo_in(s: &Store, slug: &str, tenant: TenantId) -> RepoId {
    s.create_repo_in_by(&new_repo(slug), tenant, &Actor::cli())
        .await
        .unwrap()
        .id
}

async fn job(s: &Store, repo: RepoId, n: u32) -> Result<Job, StoreError> {
    s.create_job(&NewJob {
        repo_id: repo,
        issue_ref: format!("example/r{}#{n}", repo.0),
        ledger_key: IssueId::parse(format!("r{}-{n}", repo.0)).unwrap(),
        source: JobSource::Github,
        priority: None,
        sla_deadline: None,
    })
    .await
}

async fn spend(s: &Store, job: &Job, usd: f64) {
    let call = LedgerEvent::ModelCall {
        tier: Tier::Core,
        model: "m".into(),
        tokens: TokenCounts::default(),
        cost_usd: usd,
    };
    s.ledger().append(&job.ledger_key, &call).await.unwrap();
}

#[tokio::test]
async fn repos_start_in_the_default_tenant_and_new_ones_are_audited() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let old = s.create_repo(&new_repo("old")).await.unwrap();
    assert_eq!(old.tenant_id, TenantId::DEFAULT);
    let acme = s
        .create_tenant_by("acme", "Acme Corp", &Actor::cli())
        .await
        .unwrap();
    assert_eq!(acme.secret_prefix().as_deref(), Some("ACME_"));
    assert!(matches!(
        s.create_tenant_by("acme", "Again", &Actor::cli()).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        s.create_tenant_by("Acme!", "Bad", &Actor::cli()).await,
        Err(StoreError::Invalid { .. })
    ));
    let a = repo_in(s, "a", acme.id).await;
    assert_eq!(s.repo(a).await.unwrap().unwrap().tenant_id, acme.id);
    let slugs: Vec<String> = s
        .tenants()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.slug)
        .collect();
    assert_eq!(slugs, ["default", "acme"]);

    // Moving a repo is refused while a job of it runs.
    job(s, old.id, 1).await.unwrap();
    s.claim_job("w", LEASE, ANY).await.unwrap().unwrap();
    assert!(matches!(
        s.set_repo_tenant_by(old.id, acme.id, &Actor::cli()).await,
        Err(StoreError::Conflict(_))
    ));
    // The audit log shows what was done to and in the tenant.
    let filter = AuditFilter {
        tenant: Some(acme.id),
        ..AuditFilter::default()
    };
    let actions: Vec<String> = s
        .audit_log_where(&filter, Page::first(10))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert_eq!(actions, ["repo.register", "tenant.create"]);
    db.drop().await;
}

#[tokio::test]
async fn a_spent_contract_holds_all_its_repos_and_no_other() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let acme = s
        .create_tenant_by("acme", "Acme", &Actor::cli())
        .await
        .unwrap();
    let a1 = repo_in(s, "a1", acme.id).await;
    let a2 = repo_in(s, "a2", acme.id).await;
    let other = repo_in(s, "b", TenantId::DEFAULT).await;
    // $10 cap; the default job reserve is $4.
    s.set_tenant_limits_by(acme.id, Some(10.0), None, &Actor::cli())
        .await
        .unwrap();
    let first = job(s, a1, 1).await.unwrap();
    let queued = job(s, a2, 1).await.unwrap();
    spend(s, &first, 7.0).await;
    assert_eq!(s.tenant_budget(acme.id).await.unwrap().spent_usd, 7.0);

    // New jobs of either of its repos are refused, saying why.
    let refused = job(s, a2, 2).await.unwrap_err();
    assert!(
        matches!(&refused, StoreError::BudgetSpent(why) if why.contains("contract")),
        "{refused}"
    );
    assert!(job(s, other, 1).await.is_ok(), "another tenant is not held");
    // Queued ones wait, saying why; the other tenant's run.
    let next = s.claim_job("w", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(next.job.repo_id, other);
    assert!(s.claim_job("w", LEASE, ANY).await.unwrap().is_none());
    let waiting = s
        .job(queued.id)
        .await
        .unwrap()
        .unwrap()
        .reason
        .unwrap_or_default();
    assert!(
        waiting.starts_with("waiting: the contract's monthly budget"),
        "{waiting}"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_contracts_job_cap_counts_over_its_repos_and_workers_are_shared() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let acme = s
        .create_tenant_by("acme", "Acme", &Actor::cli())
        .await
        .unwrap();
    let globex = s
        .create_tenant_by("globex", "Globex", &Actor::cli())
        .await
        .unwrap();
    let a1 = repo_in(s, "a1", acme.id).await;
    let a2 = repo_in(s, "a2", acme.id).await;
    let g = repo_in(s, "g", globex.id).await;
    // Acme queued first, twice; Globex after.
    job(s, a1, 1).await.unwrap();
    job(s, a2, 1).await.unwrap();
    job(s, a1, 2).await.unwrap();
    let gj = job(s, g, 1).await.unwrap();
    // The oldest goes first, then the tenant running fewer: Globex's,
    // though Acme's two came before it.
    let first = s.claim_job("w", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(first.job.repo_id, a1);
    let second = s.claim_job("w", LEASE, ANY).await.unwrap().unwrap();
    assert_eq!(second.job.id, gj.id, "workers are shared between contracts");

    // At most one Acme job at once: the rest wait while one runs.
    s.set_tenant_limits_by(acme.id, None, Some(1), &Actor::cli())
        .await
        .unwrap();
    assert!(s.claim_job("w", LEASE, ANY).await.unwrap().is_none());
    db.drop().await;
}

#[tokio::test]
async fn tenant_admins_are_listed_with_their_tenants_repos() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let acme = s
        .create_tenant_by("acme", "Acme", &Actor::cli())
        .await
        .unwrap();
    let a = repo_in(s, "a", acme.id).await;
    repo_in(s, "b", TenantId::DEFAULT).await;
    let user = s
        .set_tenant_admin_by(
            acme.id,
            UserKind::Person,
            "ops@acme.test",
            true,
            &Actor::cli(),
        )
        .await
        .unwrap();
    let tenants = s.tenants_administered(user.id).await.unwrap();
    assert_eq!(tenants, [acme.id]);
    assert_eq!(s.repos_of_tenants(&tenants).await.unwrap(), [a]);
    s.set_tenant_admin_by(
        acme.id,
        UserKind::Person,
        "ops@acme.test",
        false,
        &Actor::cli(),
    )
    .await
    .unwrap();
    assert!(s.tenants_administered(user.id).await.unwrap().is_empty());
    db.drop().await;
}
