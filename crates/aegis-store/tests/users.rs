//! Users, roles, sessions, tokens and the audit log (#29).

use aegis_ledger::{IssueId, Review, ReviewVerdict};
use aegis_store::testing::test_db;
use aegis_store::{
    Actor, ActorKind, Delivered, JobSource, NewDelivery, NewJob, NewRepo, Page, RepoId, RepoMode,
    RepoRole, Store, StoreError, UserKind, WebhookSource,
};
use serde_json::json;

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo_by(
        &NewRepo {
            slug: slug.into(),
            url: format!("https://github.com/x/{slug}"),
            default_branch: "main".into(),
            language: "rust".into(),
            mode: RepoMode::Poc,
            max_concurrent_jobs: 1,
            config: json!({ "budget": 1.0 }),
        },
        &Actor::cli(),
    )
    .await
    .unwrap()
    .id
}

fn new_job(repo: RepoId, key: &str) -> NewJob {
    NewJob {
        repo_id: repo,
        issue_ref: format!("x/y#{key}"),
        ledger_key: IssueId::parse(key).unwrap(),
        source: JobSource::Github,
    }
}

async fn actions(s: &Store) -> Vec<String> {
    let mut all: Vec<String> = s
        .audit_log(None, Page::first(Page::MAX_LIMIT))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|e| e.action)
        .collect();
    all.reverse();
    all
}

#[tokio::test]
async fn people_are_found_by_subject_then_by_the_email_they_were_granted() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let granted = s
        .set_repo_role(
            UserKind::Person,
            "Dev@X.test",
            a,
            Some(RepoRole::Reviewer),
            &Actor::cli(),
        )
        .await
        .unwrap();
    assert_eq!(granted.name, "dev@x.test");
    // Unverified, the e-mail takes over no one granted by it.
    assert!(matches!(
        s.sign_in("sub-1", "dev@x.test", false).await,
        Err(StoreError::Conflict(_))
    ));
    let first = s.sign_in("sub-1", "dev@x.test", true).await.unwrap();
    assert_eq!(first.id, granted.id);
    // The SSO's subject wins over a changed e-mail.
    let again = s.sign_in("sub-1", "renamed@x.test", true).await.unwrap();
    assert_eq!(again.id, granted.id);
    assert_eq!(
        s.repo_roles(again.id).await.unwrap(),
        [(a, RepoRole::Reviewer)]
    );
    // Another subject with the same e-mail is someone else.
    assert!(matches!(
        s.sign_in("sub-2", "dev@x.test", true).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        s.sign_in("sub-3", "not an email", true).await,
        Err(StoreError::Invalid { .. })
    ));
    // A new person needs no verified e-mail; taking a role away from no
    // one creates no one.
    s.sign_in("sub-4", "new@x.test", false).await.unwrap();
    assert!(matches!(
        s.set_repo_role(UserKind::Person, "typo@x.test", a, None, &Actor::cli())
            .await,
        Err(StoreError::NotFound(_))
    ));
    db.drop().await;
}

#[tokio::test]
async fn sessions_and_tokens_are_found_by_hash_until_they_end() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let user = s.sign_in("sub-1", "dev@x.test", true).await.unwrap();
    let (live, old) = ([1u8; 32], [2u8; 32]);
    let csrf = "c".repeat(64);
    let soon = chrono::Utc::now() + chrono::Duration::hours(1);
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    s.start_session(&user, &live, &csrf, soon).await.unwrap();
    s.start_session(&user, &old, &csrf, past).await.unwrap();
    let session = s.session(&live).await.unwrap().unwrap();
    assert_eq!((session.user.id, &session.csrf_token), (user.id, &csrf));
    assert!(s.session(&old).await.unwrap().is_none(), "expired");
    s.end_session(&live, &Actor::user(&user, ActorKind::User))
        .await
        .unwrap();
    assert!(s.session(&live).await.unwrap().is_none());
    s.start_session(&user, &live, &csrf, soon).await.unwrap();
    let ended = s
        .end_sessions_of(UserKind::Person, "dev@x.test", &Actor::cli())
        .await
        .unwrap();
    assert_eq!(ended, 1);
    assert!(s.session(&live).await.unwrap().is_none());

    let token = [3u8; 32];
    let service = s.create_token("ci", &token, &Actor::cli()).await.unwrap();
    assert_eq!(s.token_user(&token).await.unwrap(), Some(service));
    assert_eq!(s.revoke_tokens("ci", &Actor::cli()).await.unwrap(), 1);
    assert_eq!(s.token_user(&token).await.unwrap(), None);
    db.drop().await;
}

#[tokio::test]
async fn every_write_is_audited_with_it_or_not_at_all() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "a").await;
    let ops = s
        .create_token("ops", &[9u8; 32], &Actor::cli())
        .await
        .unwrap();
    let by = Actor::user(&ops, ActorKind::Token);
    let job = s.create_job_by(&new_job(a, "a-1"), &by).await.unwrap();
    // A refused write leaves no entry.
    assert!(matches!(
        s.create_job_by(&new_job(a, "a-1"), &by).await,
        Err(StoreError::Conflict(_))
    ));
    s.request_cancel_by(job.id, &by).await.unwrap();
    assert!(s.request_cancel_by(job.id, &by).await.is_err());
    s.retry_job_by(job.id, &by).await.unwrap();
    let delivery = NewDelivery {
        source: WebhookSource::Github,
        delivery_id: "d-1".into(),
        event: "issues".into(),
    };
    let delivered = s.deliver_job(&delivery, &new_job(a, "a-2")).await.unwrap();
    assert!(matches!(delivered, Delivered::Queued(_)));
    let review = Review {
        verdict: ReviewVerdict::Accepted,
        lines_edited: 0,
        review_minutes: 2.0,
        broke_tests: false,
    };
    s.record_review(&job, review, false, &by).await.unwrap();
    assert!(matches!(
        s.record_review(&job, review, false, &by).await,
        Err(StoreError::Conflict(_))
    ));
    let record = s.record(&job.ledger_key).await.unwrap().unwrap();
    assert_eq!(record.review, Some(review));

    assert_eq!(
        actions(s).await,
        [
            "repo.register",
            "token.create",
            "job.create",
            "job.cancel",
            "job.retry",
            "job.create",
            "review.submit",
        ]
    );
    let entries = s.audit_log(Some(a), Page::first(10)).await.unwrap().items;
    let webhook = &entries[1];
    assert_eq!(
        (webhook.actor.kind, webhook.actor.name.as_str()),
        (ActorKind::Webhook, "github")
    );
    assert_eq!(entries[0].actor.name, "ops");
    assert_eq!(entries[0].job_id, Some(job.id));

    // Append-only, even for the platform's own database user.
    assert!(
        db.raw("UPDATE audit_log SET actor_name = 'x'")
            .await
            .is_err()
    );
    assert!(db.raw("DELETE FROM audit_log").await.is_err());
    assert_eq!(actions(s).await.len(), 7);
    db.drop().await;
}
