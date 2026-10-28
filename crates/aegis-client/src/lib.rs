//! `aegis-job` (#231): a developer gives aegis-code jobs and follows them
//! from the terminal, with a token of their own from the dashboard
//! (`/me/tokens`). It only calls `aegis-server`'s `/api/v1`; it runs no
//! agent, gate or model. ADR 0006.

pub mod api;
pub mod cli;
pub mod config;
