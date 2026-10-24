//! For tests (feature `testing`): a fresh database per test on the server
//! named by `AEGIS_TEST_DATABASE_URL`. Without it the tests are skipped,
//! unless `AEGIS_REQUIRE_POSTGRES=1` (CI), where a missing server is a
//! failure.

use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::Store;
use sqlx::postgres::PgConnectOptions;
use sqlx::{AssertSqlSafe, ConnectOptions, Connection};

pub struct TestDb {
    pub store: Store,
    admin: PgConnectOptions,
    name: String,
}

/// `None` (after saying so) when no server is configured.
pub async fn test_db() -> Option<TestDb> {
    let Ok(url) = std::env::var("AEGIS_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("AEGIS_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "AEGIS_REQUIRE_POSTGRES=1 but AEGIS_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: set AEGIS_TEST_DATABASE_URL to run the Postgres tests");
        return None;
    };
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // Process, test and clock: unique even with other machines on the
    // same server.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let name = format!(
        "aegis_test_{}_{}_{nanos}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let admin = PgConnectOptions::from_str(&url).expect("AEGIS_TEST_DATABASE_URL");
    let mut conn = admin.connect().await.expect("connect to the test server");
    // The name is ours (`aegis_test_…`), not input.
    sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&mut conn)
        .await
        .expect("create the test database");
    conn.close().await.unwrap();
    let store = Store::connect_with(admin.clone().database(&name))
        .await
        .expect("connect and migrate");
    Some(TestDb { store, admin, name })
}

impl TestDb {
    /// Drops the database. A failed test leaves its database behind; CI's
    /// server is thrown away, a local one can drop `aegis_test_*` by hand.
    pub async fn drop(self) {
        self.store.close().await;
        let mut conn = self.admin.connect().await.unwrap();
        sqlx::raw_sql(AssertSqlSafe(format!(
            "DROP DATABASE {} WITH (FORCE)",
            self.name
        )))
        .execute(&mut conn)
        .await
        .unwrap();
    }
}
