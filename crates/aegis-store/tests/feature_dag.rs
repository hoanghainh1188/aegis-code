//! The feature lane's job model (#675, ADR 0019): sub-jobs claimed in the
//! order their dependencies allow, without holding back other jobs; a plan
//! that waits for approval, and the approval that queues it again.

use std::time::Duration;

use aegis_ledger::{IssueId, Outcome};
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, AttemptEnd, Delivered, Job, JobId, JobSource, JobStatus, ModeName, NewDelivery, NewJob,
    NewRepo, RepoId, Store, StoreError, WebhookSource,
};

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/example/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: ModeName::POC,
        max_concurrent_jobs: 10,
        config: serde_json::json!({}),
    })
    .await
    .unwrap()
    .id
}

fn job(repo_id: RepoId, issue: u32, key: &str) -> NewJob {
    NewJob {
        repo_id,
        issue_ref: format!("example/shop#{issue}"),
        ledger_key: IssueId::parse(key).unwrap(),
        source: JobSource::Github,
        priority: None,
        sla_deadline: None,
    }
}

/// The next job claimed, if any.
async fn claim(s: &Store) -> Option<aegis_store::Claim> {
    s.claim_job("w", Duration::from_secs(60), 10).await.unwrap()
}

async fn claim_id(s: &Store) -> Option<JobId> {
    claim(s).await.map(|c| c.job.id)
}

fn done(outcome: Outcome) -> AttemptEnd {
    AttemptEnd::Done {
        outcome,
        reason: None,
        pr_url: (outcome == Outcome::PrOpened).then(|| "https://github.com/x/y/pull/1".into()),
        patch: None,
        diagnosis: None,
    }
}

/// Claims the job `id` and ends it with `outcome`.
async fn run(s: &Store, id: JobId, outcome: Outcome) -> Job {
    let c = claim(s).await.expect("a job to claim");
    assert_eq!(c.job.id, id, "claimed {} instead", c.job.id.0);
    s.finish_attempt(&c.run, "w", &done(outcome)).await.unwrap()
}

async fn sub(s: &Store, parent: JobId, deps: &[JobId], new: NewJob) -> Job {
    s.create_sub_job_by(parent, deps, &new, &Actor::system())
        .await
        .unwrap()
}

#[tokio::test]
async fn sub_jobs_run_in_dependency_order_without_holding_back_other_jobs() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s, "shop").await;

    // The plan waits for a person: no lease, not failed, not claimed.
    let root = s.create_job(&job(shop, 1, "shop-1")).await.unwrap();
    let waiting = run(s, root.id, Outcome::AwaitingApproval).await;
    assert_eq!(
        (waiting.status, waiting.outcome, waiting.locked_by),
        (JobStatus::Done, Some(Outcome::AwaitingApproval), None)
    );
    assert_eq!(claim_id(s).await, None);
    assert_eq!(s.approval(root.id).await.unwrap(), None);

    let approved = s.approve_job_by(root.id, &Actor::system()).await.unwrap();
    assert_eq!(
        (approved.status, approved.outcome),
        (JobStatus::Queued, None)
    );
    assert_eq!(s.approval(root.id).await.unwrap().unwrap().by, "aegis");
    let err = s
        .approve_job_by(root.id, &Actor::system())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err}");

    // While the plan runs, its sub-jobs share its ticket.
    let planning = claim(s).await.unwrap();
    assert_eq!(planning.job.id, root.id);
    let a = sub(s, root.id, &[], job(shop, 1, "shop-1-a")).await;
    let b = sub(s, root.id, &[a.id], job(shop, 1, "shop-1-b")).await;
    let c = sub(s, root.id, &[a.id, b.id, a.id], job(shop, 1, "shop-1-c")).await;
    s.finish_attempt(&planning.run, "w", &done(Outcome::PrOpened))
        .await
        .unwrap();
    assert_eq!(
        s.job_links(c.id).await.unwrap().depends_on,
        vec![a.id, b.id]
    );
    assert_eq!(s.job_links(a.id).await.unwrap().parent, Some(root.id));
    let other = s.create_job(&job(shop, 2, "shop-2")).await.unwrap();

    // `b` and `c` are older than `other`, but blocked: `other` goes on.
    let a_run = claim(s).await.unwrap();
    assert_eq!(a_run.job.id, a.id);
    assert_eq!(claim_id(s).await, Some(other.id));
    assert_eq!(claim_id(s).await, None);

    // `a` opens its PR: `b` goes, then `c`, which needs both.
    s.finish_attempt(&a_run.run, "w", &done(Outcome::PrOpened))
        .await
        .unwrap();
    let b_run = claim(s).await.unwrap();
    assert_eq!(b_run.job.id, b.id);
    assert_eq!(claim_id(s).await, None, "c waits for b");
    s.finish_attempt(&b_run.run, "w", &done(Outcome::PrOpened))
        .await
        .unwrap();
    run(s, c.id, Outcome::PrOpened).await;
    let subs: Vec<JobId> = s
        .sub_jobs(root.id)
        .await
        .unwrap()
        .iter()
        .map(|j| j.id)
        .collect();
    assert_eq!(subs, vec![a.id, b.id, c.id]);
    db.drop().await;
}

#[tokio::test]
async fn a_sub_job_that_stops_without_a_pr_holds_back_its_dependents() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s, "shop").await;
    let root = s.create_job(&job(shop, 1, "shop-1")).await.unwrap();
    run(s, root.id, Outcome::PrOpened).await;
    let a = sub(s, root.id, &[], job(shop, 1, "shop-1-a")).await;
    let b = sub(s, root.id, &[a.id], job(shop, 1, "shop-1-b")).await;
    run(s, a.id, Outcome::StoppedSteps).await;
    assert_eq!(claim_id(s).await, None);
    assert_eq!(
        s.job(b.id).await.unwrap().unwrap().status,
        JobStatus::Queued
    );
    db.drop().await;
}

#[tokio::test]
async fn a_failed_sub_job_run_again_keeps_its_place() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s, "shop").await;
    let root = s.create_job(&job(shop, 1, "shop-1")).await.unwrap();
    let a = sub(s, root.id, &[], job(shop, 1, "shop-1-a")).await;
    let b = sub(s, root.id, &[a.id], job(shop, 1, "shop-1-b")).await;

    // The root runs first, then `a` fails on the platform.
    let planning = claim(s).await.unwrap();
    assert_eq!(planning.job.id, root.id);
    let a_run = claim(s).await.unwrap();
    assert_eq!(a_run.job.id, a.id);
    s.finish_attempt(
        &a_run.run,
        "w",
        &AttemptEnd::Failed {
            error: "the sandbox died".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(claim_id(s).await, None, "b waits for a");

    // Retried while its parent is still live: under the parent, not a
    // second live job of the ticket.
    let again = s.retry_job_by(a.id, &Actor::system()).await.unwrap();
    let links = s.job_links(again.id).await.unwrap();
    assert_eq!(links.parent, Some(root.id));
    assert_eq!(s.job_links(b.id).await.unwrap().depends_on, vec![again.id]);
    run(s, again.id, Outcome::PrOpened).await;
    run(s, b.id, Outcome::PrOpened).await;
    s.finish_attempt(&planning.run, "w", &done(Outcome::PrOpened))
        .await
        .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn a_plan_waiting_for_approval_keeps_its_ticket() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s, "shop").await;
    let root = s.create_job(&job(shop, 1, "shop-1")).await.unwrap();
    run(s, root.id, Outcome::AwaitingApproval).await;

    // The label put again: the plan is the ticket's live job.
    let delivery = NewDelivery {
        source: WebhookSource::Github,
        delivery_id: "d1".into(),
        event: "issues".into(),
    };
    assert_eq!(
        s.deliver_job(&delivery, &job(shop, 1, "shop-1-again"))
            .await
            .unwrap(),
        Delivered::AlreadyLive(root.id)
    );

    // A job made for the ticket by other means wins: the plan cannot be
    // queued next to it.
    let other = s.create_job(&job(shop, 1, "shop-1-cli")).await.unwrap();
    let err = s
        .approve_job_by(root.id, &Actor::system())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err}");
    run(s, other.id, Outcome::PrOpened).await;
    s.approve_job_by(root.id, &Actor::system()).await.unwrap();

    let err = s
        .approve_job_by(JobId(9999), &Actor::system())
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "{err}");
    db.drop().await;
}

#[tokio::test]
async fn sub_jobs_are_checked_when_made() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let shop = repo(s, "shop").await;
    let cart = repo(s, "cart").await;
    let root = s.create_job(&job(shop, 1, "shop-1")).await.unwrap();
    let a = sub(s, root.id, &[], job(shop, 1, "shop-1-a")).await;
    let other_root = s.create_job(&job(shop, 2, "shop-2")).await.unwrap();
    let stranger = sub(s, other_root.id, &[], job(shop, 2, "shop-2-a")).await;

    let make = |parent: JobId, deps: Vec<JobId>, new: NewJob| async move {
        s.create_sub_job_by(parent, &deps, &new, &Actor::system())
            .await
            .unwrap_err()
    };
    let cases = [
        (
            make(root.id, vec![], job(cart, 1, "cart-1-a")).await,
            "parent",
        ),
        (
            make(a.id, vec![], job(shop, 1, "shop-1-aa")).await,
            "parent",
        ),
        (
            make(root.id, vec![stranger.id], job(shop, 1, "shop-1-b")).await,
            "depends_on",
        ),
        (
            make(root.id, vec![root.id], job(shop, 1, "shop-1-c")).await,
            "depends_on",
        ),
    ];
    for (err, field) in cases {
        assert!(
            matches!(err, StoreError::Invalid { field: f, .. } if f == field),
            "{err}"
        );
    }
    let err = make(JobId(9999), vec![], job(shop, 1, "shop-1-d")).await;
    assert!(matches!(err, StoreError::NotFound(_)), "{err}");
    assert_eq!(s.sub_jobs(root.id).await.unwrap().len(), 1);
    db.drop().await;
}
