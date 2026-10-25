//! Rollback runs the previous image on a schema the newer one expanded
//! (#176, `docs/runbook`): the store starts on it, but not on a migration
//! it knows that was changed.

use aegis_store::Store;
use aegis_store::testing::test_db;

#[tokio::test]
async fn an_older_binary_starts_on_a_schema_a_newer_one_expanded() {
    let Some(db) = test_db().await else { return };
    db.raw(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
         VALUES (29991231000000, 'from a newer release', true, '\\x00', 0)",
    )
    .await
    .unwrap();
    let store = Store::connect_with(db.options()).await;
    assert!(store.is_ok(), "{:?}", store.err());
    store.unwrap().close().await;
    db.drop().await;
}

#[tokio::test]
async fn a_changed_migration_it_knows_still_stops_it() {
    let Some(db) = test_db().await else { return };
    db.raw(
        "UPDATE _sqlx_migrations SET checksum = '\\x00'
         WHERE version = (SELECT min(version) FROM _sqlx_migrations)",
    )
    .await
    .unwrap();
    let refused = Store::connect_with(db.options()).await;
    assert!(refused.is_err(), "a changed migration was let through");
    db.drop().await;
}
