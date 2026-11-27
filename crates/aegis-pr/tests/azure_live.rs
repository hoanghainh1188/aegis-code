//! #642, measured on a real Azure DevOps organization: what Azure says of
//! each identity's right to write to a repository, and whether the writer
//! check decides it as expected. Ignored by default; run it by hand:
//!
//! ```text
//! AEGIS_AZURE_LIVE_API=https://dev.azure.com/<org> \
//! AEGIS_AZURE_LIVE_PROJECT=<project> AEGIS_AZURE_LIVE_REPO=<repo> \
//! AEGIS_AZURE_LIVE_TOKEN=<PAT: Code (Read), Identity (Read), Security (Manage)> \
//! AEGIS_AZURE_LIVE_CASES='nested=<guid>:true,denied=<guid>:false,direct=<guid>:true' \
//! cargo test -p aegis-pr --test azure_live -- --ignored --nocapture
//! ```
//!
//! Each case is `name=<identity GUID>:<whether it may write>`. The usual
//! three:
//! - `nested`: allowed only through a group inside a group, a team inside
//!   Contributors, with no entry of its own on the repository;
//! - `denied`: in Contributors, denied `Contribute` on the repository;
//! - `direct`: in no writer group, allowed `Contribute` on the repository.
//!
//! An identity's GUID is its `id` in
//! `https://vssps.dev.azure.com/<org>/_apis/identities?searchFilter=General&filterValue=<email>&api-version=7.1`.
//! The table it prints holds no token and no name, only the case names,
//! descriptors' kinds and the permission bits: it can go on the issue.

use aegis_pr::AzureDevOps;

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

/// The cases of `AEGIS_AZURE_LIVE_CASES`: name, identity, expected.
fn cases(text: &str) -> Vec<(String, String, bool)> {
    text.split(',')
        .filter(|c| !c.trim().is_empty())
        .map(|case| {
            let (name, rest) = case.trim().split_once('=').expect("name=<guid>:<bool>");
            let (id, expected) = rest.split_once(':').expect("<guid>:<bool>");
            let expected = expected.parse().expect("true or false");
            (name.to_owned(), id.to_owned(), expected)
        })
        .collect()
}

#[tokio::test]
#[ignore = "needs a real Azure DevOps organization: see the module"]
async fn the_writer_check_on_a_real_organization() {
    let azure = AzureDevOps::new(
        &var("AEGIS_AZURE_LIVE_API"),
        &var("AEGIS_AZURE_LIVE_PROJECT"),
        &var("AEGIS_AZURE_LIVE_REPO"),
        &var("AEGIS_AZURE_LIVE_TOKEN"),
    )
    .unwrap();
    let cases = cases(&var("AEGIS_AZURE_LIVE_CASES"));
    assert!(!cases.is_empty(), "no case given");
    println!(
        "| case | descriptor kind | ACE allow/deny | effective allow/deny | in writer group | writes | expected |"
    );
    println!("|---|---|---|---|---|---|---|");
    let mut wrong = Vec::new();
    for (name, id, expected) in &cases {
        let e = azure.write_evidence(id).await.unwrap();
        let kind = e
            .descriptor
            .as_deref()
            .and_then(|d| d.split(';').next())
            .unwrap_or("none");
        let (ace, effective) = match &e.entry {
            Ok(entry) => (
                format!("{}/{}", entry.allow, entry.deny),
                entry
                    .effective
                    .map_or("none".into(), |(a, d)| format!("{a}/{d}")),
            ),
            Err(why) => ("-".into(), format!("none: {why}")),
        };
        println!(
            "| {name} | {kind} | {ace} | {effective} | {} | {} | {expected} |",
            e.writer_group.as_ref().map_or("none".into(), |g| format!(
                "{} via {}",
                g.name,
                g.via.join(" > ")
            )),
            e.writes
        );
        if e.writes != *expected {
            wrong.push(name.clone());
        }
    }
    assert!(wrong.is_empty(), "decided wrong: {wrong:?}");
}

#[test]
fn cases_are_read_from_the_variable() {
    assert_eq!(
        cases("nested=a:true, denied=b:false,"),
        vec![
            ("nested".to_owned(), "a".to_owned(), true),
            ("denied".to_owned(), "b".to_owned(), false)
        ]
    );
}
