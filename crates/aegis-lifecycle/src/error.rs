use thiserror::Error;

/// The lifecycle could not run. An issue that runs and stops (budget,
/// Expert failed, cancelled) is an [`crate::IssueReport`], not an error.
#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("cannot read the issue: {0}")]
    Issue(String),

    #[error("git {args}: {message}")]
    Git { args: String, message: String },

    #[error(transparent)]
    Ledger(#[from] aegis_ledger::LedgerError),

    #[error(transparent)]
    Lang(#[from] aegis_lang::LangError),

    #[error(transparent)]
    Aci(#[from] aegis_aci::AciError),

    #[error(transparent)]
    Gate(#[from] aegis_gate::GateError),

    #[error(transparent)]
    Pr(#[from] aegis_pr::PrError),

    #[error(transparent)]
    Tools(#[from] aegis_orchestrator::McpToolsError),

    #[error("I/O error on `{path}`: {message}")]
    Io { path: String, message: String },
}

impl LifecycleError {
    pub(crate) fn io(path: &std::path::Path, e: &std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        }
    }
}
