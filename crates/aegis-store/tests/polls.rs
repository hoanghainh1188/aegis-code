//! A poll's schedule in the database (#26): one caller per period.

use std::time::Duration;

use aegis_store::testing::test_db;

#[tokio::test]
async fn one_caller_polls_per_period() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let hour = Duration::from_secs(3600);
    let claims = futures_claims(s, hour).await;
    assert_eq!(claims.iter().filter(|c| **c).count(), 1, "{claims:?}");
    assert!(!s.claim_poll("trackers", hour).await.unwrap());
    // Another poll has its own schedule.
    assert!(s.claim_poll("sonar", hour).await.unwrap());

    // Due again once its period is over.
    let now = Duration::ZERO;
    assert!(s.claim_poll("often", now).await.unwrap());
    assert!(s.claim_poll("often", now).await.unwrap());
}

async fn futures_claims(s: &aegis_store::Store, every: Duration) -> Vec<bool> {
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let s = s.clone();
        tasks.spawn(async move { s.claim_poll("trackers", every).await.unwrap() });
    }
    tasks.join_all().await
}
