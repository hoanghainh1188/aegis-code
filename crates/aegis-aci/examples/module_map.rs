//! Prints the module map of a repo, as it goes into the system prompt.
//!
//! `cargo run -p aegis-aci --example module_map -- <repo> <language>`

use std::path::Path;

use aegis_lang::{Language, ProfileOverrides, Profiles};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [repo, id] = args.as_slice() else {
        return Err("usage: module_map <repo> <language>".into());
    };
    let language = Language::ALL
        .into_iter()
        .find(|l| l.id() == id)
        .ok_or_else(|| format!("unknown language `{id}`"))?;
    let profile = Profiles::builtin()?
        .get(language)?
        .resolve(&ProfileOverrides::default())?;
    let ws = aegis_aci::Workspace::new(Path::new(repo), profile)?;
    let started = std::time::Instant::now();
    let map = aegis_aci::module_summary(&ws)?;
    eprintln!("{} chars in {:?}", map.len(), started.elapsed());
    print!("{map}");
    Ok(())
}
