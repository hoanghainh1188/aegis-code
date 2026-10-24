use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),

    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("{0} not found")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// The repo's monthly budget is spent (#31): the reason, to show.
    #[error("{0}")]
    BudgetSpent(String),

    #[error("invalid {field}: {message}")]
    Invalid {
        field: &'static str,
        message: String,
    },

    #[error("unexpected `{value}` in {column}")]
    Decode { column: &'static str, value: String },

    #[error(transparent)]
    Ledger(#[from] aegis_ledger::LedgerError),
}

impl StoreError {
    /// Constraint violations as the caller's error, the rest as `Db`.
    pub(crate) fn from_db(error: sqlx::Error) -> Self {
        let Some(db) = error.as_database_error() else {
            return Self::Db(error);
        };
        let what = db.constraint().unwrap_or("a constraint").to_owned();
        match db.code().as_deref() {
            Some("23505") => Self::Conflict(format!("{what} already taken")),
            Some("23503") => Self::NotFound(format!("the row {what} refers to")),
            Some("23514") => Self::Invalid {
                field: "row",
                message: format!("{what} does not hold"),
            },
            _ => Self::Db(error),
        }
    }
}
