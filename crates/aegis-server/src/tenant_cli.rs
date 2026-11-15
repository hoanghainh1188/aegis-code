//! Tenants (contracts, #525) from the server's command line, audited as
//! `cli`:
//!
//! ```text
//! aegis-server --config <file> tenant <command>
//!   add <slug> <name…>                 # a new contract
//!   list                               # each with its caps and this month's spend
//!   limits <slug> [--budget <usd>|none] [--max-jobs <n>|none]
//!   admin <who> <slug>                 # an admin of its repos (roles, pauses, budgets)
//!   revoke-admin <who> <slug>
//!   move <repo-slug> <slug>            # a repo into the tenant; its secrets must be named for it
//!   installations                      # the GitHub App's, with the tenant each is bound to
//!   bind-installation <id> <slug>      # its repos' tokens only for this tenant (ADR 0017 §6)
//!   unbind-installation <id>           # no token for it until bound again
//! ```
//!
//! A repo is added straight into a tenant with `tenant = "<slug>"` in its
//! repo file (`repo add`). The secrets a repo of a tenant other than
//! `default` names start with the tenant's prefix (`ACME_` for `acme`),
//! and its clone and jobs live under `<state_dir>/tenants/<id>`.

use aegis_lifecycle::check_tenant_secrets;
use aegis_store::{Actor, Store, StoreError, Tenant};

use crate::auth::cli::kind_of;

pub const USAGE: &str = "\
aegis-server --config <file> tenant <command>, the command one of:
  add <slug> <name…>
  list
  limits <slug> [--budget <usd>|none] [--max-jobs <n>|none]
  admin <who> <slug>
  revoke-admin <who> <slug>
  move <repo-slug> <slug>
  installations
  bind-installation <id> <slug>
  unbind-installation <id>";

pub fn is_command(first: &str) -> bool {
    first == "tenant"
}

async fn tenant(store: &Store, slug: &str) -> Result<Tenant, String> {
    store
        .tenant_by_slug(slug)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no tenant `{slug}`"))
}

/// `--budget` and `--max-jobs`, each a number or `none`; unchanged when
/// not given.
fn limits(rest: &[&str], was: &Tenant) -> Result<(Option<f64>, Option<u32>), String> {
    let (mut budget, mut jobs) = (was.monthly_budget_usd, was.max_concurrent_jobs);
    let mut rest = rest;
    while let Some((flag, tail)) = rest.split_first() {
        let (value, tail) = tail
            .split_first()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        rest = tail;
        match *flag {
            "--budget" if *value == "none" => budget = None,
            "--budget" => {
                budget = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--budget `{value}` is not a number of dollars"))?,
                );
            }
            "--max-jobs" if *value == "none" => jobs = None,
            "--max-jobs" => {
                jobs = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--max-jobs `{value}` is not a number"))?,
                );
            }
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    Ok((budget, jobs))
}

fn shown(cap: Option<f64>) -> String {
    cap.map_or("no cap".into(), |c| format!("${c:.2}"))
}

pub async fn run(store: &Store, args: &[String]) -> Result<String, String> {
    let actor = Actor::cli();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let e = |e: StoreError| e.to_string();
    match args.as_slice() {
        ["add", slug, name @ ..] if !name.is_empty() => {
            let t = store
                .create_tenant_by(slug, &name.join(" "), &actor)
                .await
                .map_err(e)?;
            Ok(format!(
                "tenant `{}` added (id {}); its repos' secrets start with `{}`",
                t.slug,
                t.id.0,
                t.secret_prefix().unwrap_or_default()
            ))
        }
        ["list"] => {
            let mut lines = Vec::new();
            for t in store.tenants().await.map_err(e)? {
                let budget = store.tenant_budget(t.id).await.map_err(e)?;
                let jobs = t
                    .max_concurrent_jobs
                    .map_or("no cap".into(), |n| n.to_string());
                lines.push(format!(
                    "{}\t{}\tbudget {} (spent ${:.2})\tjobs at once {jobs}",
                    t.slug,
                    t.name,
                    shown(t.monthly_budget_usd),
                    budget.spent_usd
                ));
            }
            Ok(lines.join("\n"))
        }
        ["limits", slug, rest @ ..] => {
            let t = tenant(store, slug).await?;
            let (budget, jobs) = limits(rest, &t)?;
            let t = store
                .set_tenant_limits_by(t.id, budget, jobs, &actor)
                .await
                .map_err(e)?;
            Ok(format!(
                "tenant `{}`: budget {}, jobs at once {}",
                t.slug,
                shown(t.monthly_budget_usd),
                t.max_concurrent_jobs
                    .map_or("no cap".into(), |n| n.to_string())
            ))
        }
        [verb @ ("admin" | "revoke-admin"), who, slug] => {
            let t = tenant(store, slug).await?;
            let admin = *verb == "admin";
            let _ = store
                .set_tenant_admin_by(t.id, kind_of(who), who, admin, &actor)
                .await
                .map_err(e)?;
            Ok(match admin {
                true => format!("{who}: admin of tenant `{}`", t.slug),
                false => format!("{who}: no longer an admin of tenant `{}`", t.slug),
            })
        }
        ["move", repo_slug, slug] => {
            let t = tenant(store, slug).await?;
            let repo = store
                .repo_by_slug(repo_slug)
                .await
                .map_err(e)?
                .ok_or_else(|| format!("no repo `{repo_slug}`"))?;
            check_tenant_secrets(&t, &repo.config).map_err(|e| e.to_string())?;
            store
                .set_repo_tenant_by(repo.id, t.id, &actor)
                .await
                .map_err(e)?;
            Ok(format!(
                "repo `{repo_slug}` is now in tenant `{}`; its next job clones it under the \
                 tenant's directory (the old clone can be removed)",
                t.slug
            ))
        }
        ["installations"] => {
            let tenants = store.tenants().await.map_err(e)?;
            let slug_of = |id: aegis_store::TenantId| {
                tenants
                    .iter()
                    .find(|t| t.id == id)
                    .map_or_else(|| id.0.to_string(), |t| t.slug.clone())
            };
            let lines: Vec<String> = store
                .installations()
                .await
                .map_err(e)?
                .into_iter()
                .map(|i| {
                    let state = match (i.removed, i.suspended) {
                        (true, _) => "removed",
                        (false, true) => "suspended",
                        (false, false) => "installed",
                    };
                    format!(
                        "{}\t{}\t{state}\t{} repos named\t{}",
                        i.id,
                        i.account,
                        i.repositories.len(),
                        i.tenant
                            .map_or("not bound".into(), |t| format!("tenant {}", slug_of(t)))
                    )
                })
                .collect();
            Ok(if lines.is_empty() {
                "no installation recorded yet".into()
            } else {
                lines.join("\n")
            })
        }
        ["bind-installation", id, slug] => {
            let id = installation_id(id)?;
            let t = tenant(store, slug).await?;
            store
                .bind_installation_by(id, Some(t.id), &actor)
                .await
                .map_err(e)?;
            Ok(format!(
                "installation {id} bound to tenant `{}`: its repos' tokens are for it only",
                t.slug
            ))
        }
        ["unbind-installation", id] => {
            let id = installation_id(id)?;
            store
                .bind_installation_by(id, None, &actor)
                .await
                .map_err(e)?;
            Ok(format!(
                "installation {id} is bound to no tenant: no token is minted for it"
            ))
        }
        _ => Err(USAGE.into()),
    }
}

fn installation_id(text: &str) -> Result<i64, String> {
    text.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("`{text}` is not an installation id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_store::TenantId;

    #[test]
    fn limits_change_only_what_is_given() {
        let was = Tenant {
            id: TenantId(2),
            slug: "acme".into(),
            name: "Acme".into(),
            monthly_budget_usd: Some(100.0),
            max_concurrent_jobs: Some(2),
            created_at: chrono::Utc::now(),
        };
        assert_eq!(limits(&[], &was).unwrap(), (Some(100.0), Some(2)));
        assert_eq!(
            limits(&["--budget", "250.5"], &was).unwrap(),
            (Some(250.5), Some(2))
        );
        assert_eq!(
            limits(&["--max-jobs", "none", "--budget", "none"], &was).unwrap(),
            (None, None)
        );
        assert!(limits(&["--budget"], &was).is_err());
        assert!(limits(&["--budget", "lots"], &was).is_err());
        assert!(limits(&["--cap", "1"], &was).is_err());
    }
}
