//! `POST /containers/create`: the body to send, rebuilt from what the
//! gate's `Hardening::args` sets (#320) and checked field by field. Any
//! other field must hold its zero value and is left out, so the daemon
//! applies its default: masked `/proc` paths, its seccomp and AppArmor
//! profiles, its log driver, no added groups or capabilities.

use std::path::{Component, Path};

use serde_json::{Map, Value, json};

use super::json::{self, object};
use super::{Policy, is_fetch_network};

/// Runtimes a sandbox may ask for: the default, and gVisor (#322).
const RUNTIMES: &[&str] = &["", "runc", "runsc"];

const CONFIG_KEYS: &[&str] = &[
    "User",
    "AttachStdin",
    "AttachStdout",
    "AttachStderr",
    "Tty",
    "OpenStdin",
    "StdinOnce",
    "Env",
    "Cmd",
    "Image",
    "WorkingDir",
    "Entrypoint",
    "HostConfig",
    "NetworkingConfig",
];

const HOST_KEYS: &[&str] = &[
    "Binds",
    "Mounts",
    "Tmpfs",
    "NetworkMode",
    "AutoRemove",
    "CapDrop",
    "SecurityOpt",
    "ReadonlyRootfs",
    "Memory",
    "MemorySwap",
    "NanoCpus",
    "PidsLimit",
    "Runtime",
    // The client's defaults, checked below and left out.
    "RestartPolicy",
    "MemorySwappiness",
];

impl Policy {
    /// The body to send for `POST /containers/create?name=<name>`.
    pub fn create(&self, body: &Value) -> Result<Value, String> {
        let config = json::kept(object(body, "the body")?, CONFIG_KEYS, "Config")?;
        let user = user(json::string(&config, "User")?)?;
        let image = json::string(&config, "Image")?;
        if image.is_empty() {
            return Err("no Image".into());
        }
        let host_in = config.get("HostConfig").ok_or("no HostConfig")?;
        let host = json::kept(object(host_in, "HostConfig")?, HOST_KEYS, "HostConfig")?;
        let mut out_host = hardening(&host)?;
        let read_only = json::bool(&host, "ReadonlyRootfs")?;
        if !read_only && !self.writable_images.iter().any(|i| i == image) {
            return Err(format!("image {image:?} must run with a read-only root"));
        }
        out_host.insert("ReadonlyRootfs".into(), read_only.into());
        defaults(&host)?;
        let runtime = json::string(&host, "Runtime")?;
        if !RUNTIMES.contains(&runtime) {
            return Err(format!("runtime {runtime:?}"));
        }
        if !runtime.is_empty() {
            out_host.insert("Runtime".into(), runtime.into());
        }
        out_host.insert("AutoRemove".into(), json::bool(&host, "AutoRemove")?.into());
        let binds = self.binds(&host)?;
        let mounts = self.mounts(&host)?;
        let tmpfs = tmpfs(&host)?;
        let any_bind = !binds.is_empty() || mounts.iter().any(|m| m["Type"] == "bind");
        let network = self.network(&host, &config, any_bind)?;
        out_host.insert("Binds".into(), binds.into());
        out_host.insert("Mounts".into(), mounts.into());
        out_host.insert("Tmpfs".into(), tmpfs.into());
        out_host.insert("NetworkMode".into(), network.clone().into());

        let mut out = Map::new();
        out.insert("Image".into(), image.into());
        out.insert("User".into(), user.into());
        for key in [
            "AttachStdin",
            "AttachStdout",
            "AttachStderr",
            "Tty",
            "OpenStdin",
            "StdinOnce",
        ] {
            out.insert(key.into(), json::bool(&config, key)?.into());
        }
        for key in ["Env", "Cmd", "Entrypoint"] {
            let list = json::strings(&config, key)?;
            if config.get(key).is_some_and(|v| !v.is_null()) {
                out.insert(key.into(), list.into());
            }
        }
        out.insert(
            "WorkingDir".into(),
            json::string(&config, "WorkingDir")?.into(),
        );
        out.insert("HostConfig".into(), Value::Object(out_host));
        out.insert(
            "NetworkingConfig".into(),
            json!({ "EndpointsConfig": { network: {} } }),
        );
        Ok(Value::Object(out))
    }

    /// `Binds` (`--volume`): `<source>:<target>[:ro]` from the workspace.
    fn binds(&self, host: &Map<String, Value>) -> Result<Vec<String>, String> {
        json::strings(host, "Binds")?
            .into_iter()
            .map(|bind| {
                let parts: Vec<&str> = bind.split(':').collect();
                match parts.as_slice() {
                    [source, target] | [source, target, "ro" | "rw"] => {
                        self.check_source(source)?;
                        check_target(target)?;
                        Ok(bind.to_owned())
                    }
                    _ => Err(format!("bind {bind:?} has options aegis does not set")),
                }
            })
            .collect()
    }

    /// `Mounts` (`--mount`): anonymous volumes, tmpfs, and binds from the
    /// workspace, with no options (a volume's driver options could mount
    /// any host path).
    fn mounts(&self, host: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let list = match host.get("Mounts") {
            None | Some(Value::Null) => return Ok(vec![]),
            Some(Value::Array(a)) => a,
            Some(_) => return Err("Mounts is not a list".into()),
        };
        list.iter()
            .map(|m| {
                let m = json::kept(
                    object(m, "mount")?,
                    &["Type", "Source", "Target", "ReadOnly"],
                    "Mounts",
                )?;
                let kind = json::string(&m, "Type")?;
                let source = json::string(&m, "Source")?;
                let target = json::string(&m, "Target")?;
                check_target(target)?;
                match kind {
                    "bind" => self.check_source(source)?,
                    "volume" | "tmpfs" if source.is_empty() => {}
                    _ => return Err(format!("mount of type {kind:?} from {source:?}")),
                }
                Ok(json!({
                    "Type": kind,
                    "Source": source,
                    "Target": target,
                    "ReadOnly": json::bool(&m, "ReadOnly")?,
                }))
            })
            .collect()
    }

    fn check_source(&self, source: &str) -> Result<(), String> {
        let path = Path::new(source);
        if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
            return Err(format!(
                "mount source {source:?} is not a plain absolute path"
            ));
        }
        // The daemon follows links: a link in the workspace to `/` would
        // mount the machine. Missing sources are refused; the daemon would
        // create them.
        let real =
            std::fs::canonicalize(path).map_err(|e| format!("mount source {source:?}: {e}"))?;
        let allowed = self
            .mount_roots
            .iter()
            .any(|root| std::fs::canonicalize(root).is_ok_and(|root| real.starts_with(root)));
        if allowed {
            Ok(())
        } else {
            Err(format!("mount source {source:?} is outside the workspace"))
        }
    }

    /// `none`, the fetch step's network, or one of [`Policy::networks`].
    /// The default bridge only without bind mounts: the egress proxy,
    /// which reaches the registries and holds nothing of a job.
    fn network(
        &self,
        host: &Map<String, Value>,
        config: &Map<String, Value>,
        any_bind: bool,
    ) -> Result<String, String> {
        let mode = json::string(host, "NetworkMode")?;
        let mode = if mode.is_empty() { "bridge" } else { mode };
        // The client names no endpoint for `--network`; any it names must
        // be the same network, with no settings (aliases, addresses).
        if let Some(net) = config.get("NetworkingConfig") {
            let net = json::kept(
                object(net, "NetworkingConfig")?,
                &["EndpointsConfig"],
                "NetworkingConfig",
            )?;
            if let Some(endpoints) = net.get("EndpointsConfig") {
                for (name, endpoint) in object(endpoints, "EndpointsConfig")? {
                    if name != mode || !json::is_zero(endpoint) {
                        return Err(format!("endpoint {name:?} besides network {mode:?}"));
                    }
                }
            }
        }
        let ok = match mode {
            "none" => true,
            "bridge" | "default" => !any_bind || self.networks.iter().any(|n| n == mode),
            n => is_fetch_network(n) || self.networks.iter().any(|m| m == n),
        };
        if ok {
            Ok(mode.to_owned())
        } else {
            Err(format!("network {mode:?} is not allowed here"))
        }
    }
}

/// The flags `Hardening::args` always sets (#320), exactly.
fn hardening(host: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    if json::strings(host, "CapDrop")? != ["ALL"] {
        return Err("CapDrop must be [\"ALL\"]".into());
    }
    let opts = json::strings(host, "SecurityOpt")?;
    if !matches!(
        opts.as_slice(),
        ["no-new-privileges" | "no-new-privileges:true"]
    ) {
        return Err("SecurityOpt must be [\"no-new-privileges\"] alone".into());
    }
    let mut out = Map::new();
    out.insert("CapDrop".into(), json!(["ALL"]));
    out.insert("SecurityOpt".into(), json!(["no-new-privileges"]));
    for key in ["Memory", "NanoCpus", "PidsLimit"] {
        let n = json::integer(host, key)?;
        if n <= 0 {
            return Err(format!("{key} must be set"));
        }
        out.insert(key.into(), n.into());
    }
    let memory = json::integer(host, "Memory")?;
    if json::integer(host, "MemorySwap")? != memory {
        return Err("MemorySwap must equal Memory".into());
    }
    out.insert("MemorySwap".into(), memory.into());
    Ok(out)
}

/// What the client sends when nothing is set: `--restart no` and the
/// kernel's swappiness.
fn defaults(host: &Map<String, Value>) -> Result<(), String> {
    if let Some(policy) = host.get("RestartPolicy") {
        let policy = json::kept(object(policy, "RestartPolicy")?, &["Name"], "RestartPolicy")?;
        if !matches!(json::string(&policy, "Name")?, "" | "no") {
            return Err("RestartPolicy is set".into());
        }
    }
    if !matches!(json::integer(host, "MemorySwappiness")?, 0 | -1) {
        return Err("MemorySwappiness is set".into());
    }
    Ok(())
}

/// `Tmpfs` (`--tmpfs`): paths in the container and their options.
fn tmpfs(host: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let Some(map) = host.get("Tmpfs").filter(|v| !v.is_null()) else {
        return Ok(Map::new());
    };
    let mut out = Map::new();
    for (path, opts) in object(map, "Tmpfs")? {
        check_target(path)?;
        let opts = opts.as_str().ok_or("Tmpfs options are not a string")?;
        // As `Hardening::args` sets them: no set-uid files, no devices.
        let flags: Vec<&str> = opts.split(',').collect();
        if !flags.contains(&"nosuid") || !flags.contains(&"nodev") {
            return Err(format!("tmpfs {path:?} must be nosuid,nodev"));
        }
        if flags.iter().any(|f| matches!(*f, "suid" | "dev")) {
            return Err(format!("tmpfs {path:?} options {opts:?}"));
        }
        out.insert(path.clone(), opts.into());
    }
    Ok(out)
}

fn check_target(target: &str) -> Result<(), String> {
    let path = Path::new(target);
    if path.is_absolute() && !path.components().any(|c| c == Component::ParentDir) {
        Ok(())
    } else {
        Err(format!(
            "mount target {target:?} is not a plain absolute path"
        ))
    }
}

/// `uid:gid`, both decimal and neither 0. The daemon reads `00` and `+0`
/// as root; plain digits leave no other way.
fn user(user: &str) -> Result<String, String> {
    let parse = |s: &str| {
        (!s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
            .filter(|n| *n != 0)
    };
    match user.split_once(':') {
        Some((uid, gid)) if parse(uid).is_some() && parse(gid).is_some() => Ok(user.to_owned()),
        _ => Err(format!(
            "container user {user:?} must be a non-root uid:gid"
        )),
    }
}
