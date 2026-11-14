//! What aegis may ask of the Docker daemon (#321): the calls of the gate,
//! the egress proxy and the ACI's containers, and nothing else. A
//! container is created only as `Hardening::args` makes one (#320), and
//! only with the job trees and caches mounted. Bodies are rebuilt from
//! the keys checked ([`json`]), never forwarded as received.

mod container;
mod json;
mod query;

use std::path::PathBuf;

use serde_json::{Value, json};

/// The names of the containers aegis creates: the gate's steps (the Sonar
/// scanner too), the egress proxy, the ACI's formatter and language
/// server. Every call on a container needs one of them.
pub const CONTAINER_PREFIXES: &[&str] = &[
    "aegis-gate-",
    "aegis-egress-",
    "aegis-format-",
    "aegis-lsp-",
    "aegis-sidecar-",
];

/// The sidecars of a gate run (#490, ADR 0013), their readiness probes,
/// and their internal network: `aegis_gate::SIDECAR_PREFIX`.
pub const SIDECAR_PREFIX: &str = "aegis-sidecar-";

/// The fetch step's internal network (`aegis_gate::Egress`).
pub const FETCH_NETWORK_PREFIX: &str = "aegis-fetch-";

/// The oldest API a call may name. Before 1.24 `start` took a
/// `HostConfig` of its own, past the checked one; before 1.41 an unset
/// `CgroupnsMode` was the host's on cgroup v1.
const MIN_API_MINOR: u32 = 44;

/// What a call is, from its method, path and query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// `/_ping` and `/version`: the client's handshake.
    Handshake,
    /// `POST /containers/create?name=<name>`: its body is checked.
    CreateContainer(String),
    /// A call on one container, allowed when it is one of aegis's.
    Container(String),
    /// `POST /containers/<id>/attach`: the one call that upgrades.
    Attach(String),
    /// `POST /networks/create`: its body is checked.
    CreateNetwork,
    /// `POST /networks/<net>/connect`: its body names the container.
    ConnectNetwork(String),
    /// `GET` and `DELETE /networks/<net>`: `docker network rm` inspects
    /// first.
    Network(String),
    /// Inspecting and pulling an image.
    Image,
}

impl Route {
    /// Whether the call has a JSON body to check; any other must be empty.
    pub fn has_body(&self) -> bool {
        matches!(
            self,
            Self::CreateContainer(_) | Self::CreateNetwork | Self::ConnectNetwork(_)
        )
    }
}

/// The call `method path?query` maps to, or why it is refused.
pub fn route(method: &str, path: &str, query: Option<&str>) -> Result<Route, String> {
    let path = strip_version(path)?;
    let refused = || Err(format!("{method} {path} is not a call aegis makes"));
    let Some(rest) = path.strip_prefix('/') else {
        return refused();
    };
    let parts: Vec<&str> = rest.split('/').collect();
    // The daemon cleans paths: `/images/../containers/x/json` would be
    // another call than the one checked.
    if path.contains('%') || parts.iter().any(|p| matches!(*p, "" | "." | "..")) {
        return Err(format!("{method} {path} is not a plain path"));
    }
    let params = |allowed: &[&str]| query::parse(query, allowed);
    let route = match (method, parts.as_slice()) {
        ("GET" | "HEAD", ["_ping"]) | ("GET", ["version"]) => {
            params(&[])?;
            Route::Handshake
        }
        ("POST", ["containers", "create"]) => {
            let q = params(&["name", "platform"])?;
            let name = q.get("name").cloned().unwrap_or_default();
            if !is_aegis_container(&name) {
                return Err(format!("container name {name:?} is not one of aegis's"));
            }
            Route::CreateContainer(name)
        }
        ("POST", ["containers", id, "attach"]) => {
            params(&["stream", "stdin", "stdout", "stderr", "logs", "detachKeys"])?;
            Route::Attach((*id).to_owned())
        }
        ("GET", ["containers", id, action]) => {
            let allowed: &[&str] = match *action {
                "json" => &["size"],
                "logs" => &[
                    "follow",
                    "stdout",
                    "stderr",
                    "since",
                    "until",
                    "timestamps",
                    "tail",
                ],
                _ => return refused(),
            };
            params(allowed)?;
            Route::Container((*id).to_owned())
        }
        ("POST", ["containers", id, action]) => {
            let allowed: &[&str] = match *action {
                "start" => &[],
                "wait" => &["condition"],
                "kill" => &["signal"],
                "stop" => &["t", "signal"],
                "resize" => &["h", "w"],
                _ => return refused(),
            };
            params(allowed)?;
            Route::Container((*id).to_owned())
        }
        ("DELETE", ["containers", id]) => {
            params(&["force", "v"])?;
            Route::Container((*id).to_owned())
        }
        ("POST", ["networks", "create"]) => {
            params(&[])?;
            Route::CreateNetwork
        }
        ("POST", ["networks", net, "connect"]) => {
            params(&[])?;
            Route::ConnectNetwork((*net).to_owned())
        }
        ("GET" | "DELETE", ["networks", net]) => {
            params(&[])?;
            Route::Network((*net).to_owned())
        }
        ("POST", ["images", "create"]) => {
            // `fromSrc` would import a tarball from a URL or the body.
            let q = params(&["fromImage", "tag", "platform"])?;
            if q.get("fromImage").is_none_or(String::is_empty) {
                return Err("pulling needs fromImage".into());
            }
            Route::Image
        }
        // An image's name has slashes: `/images/<registry>/<name>/json`.
        ("GET", ["images", .., "json"]) if parts.len() >= 3 => {
            params(&[])?;
            Route::Image
        }
        _ => return refused(),
    };
    Ok(route)
}

/// `/v1.56/containers/create` → `/containers/create`; no version is the
/// daemon's own. Versions before [`MIN_API_MINOR`] are refused.
fn strip_version(path: &str) -> Result<&str, String> {
    // `/version` is not a version.
    let Some(rest) = path
        .strip_prefix("/v")
        .filter(|r| r.starts_with(|c: char| c.is_ascii_digit()))
    else {
        return Ok(path);
    };
    let (version, tail) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let minor = version
        .strip_prefix("1.")
        .filter(|m| !m.is_empty() && m.chars().all(|c| c.is_ascii_digit()))
        .and_then(|m| m.parse::<u32>().ok())
        .ok_or_else(|| format!("API version {version:?}"))?;
    if minor < MIN_API_MINOR {
        return Err(format!(
            "API version 1.{minor} is older than 1.{MIN_API_MINOR}"
        ));
    }
    Ok(tail)
}

/// Whether `name` is one of aegis's containers. Docker reports names with
/// a leading `/`.
pub fn is_aegis_container(name: &str) -> bool {
    let name = name.strip_prefix('/').unwrap_or(name);
    CONTAINER_PREFIXES.iter().any(|p| {
        name.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(is_name_char))
    })
}

fn is_fetch_network(name: &str) -> bool {
    name.strip_prefix(FETCH_NETWORK_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(is_name_char))
}

/// A sidecar network, or a sidecar container: they share the prefix.
fn is_sidecar(name: &str) -> bool {
    let name = name.strip_prefix('/').unwrap_or(name);
    name.strip_prefix(SIDECAR_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(is_name_char))
}

/// A network a container may join by name: the fetch step's or a gate
/// run's sidecars'. Both are internal (see [`Policy::network_create`]).
fn is_internal_network(name: &str) -> bool {
    is_fetch_network(name) || is_sidecar(name)
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

/// The parts of the policy the deployment sets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Host directories a container may mount from: the workspace (job
    /// trees and caches). Compared after resolving symbolic links.
    pub mount_roots: Vec<PathBuf>,
    /// Networks a container may join besides `none` and the fetch step's,
    /// e.g. the Sonar scanner's (`[sonar] network`).
    pub networks: Vec<String>,
    /// Images that may run with a writable root file system: the Sonar
    /// scanner's.
    pub writable_images: Vec<String>,
    /// Images a sidecar may run (#490): the services the deployment's
    /// repos declare, such as `postgres:16-alpine`. A sidecar runs no
    /// other image, and nothing but a sidecar gets a network alias.
    pub sidecar_images: Vec<String>,
}

impl Policy {
    /// The body to send for `POST /networks/create`: only internal bridge
    /// networks, the fetch step's and the sidecars'.
    pub fn network_create(&self, body: &Value) -> Result<Value, String> {
        let obj = json::object(body, "network")?;
        let mut rest = obj.clone();
        // What the client always sends: the default IPAM, nothing set.
        if let Some(ipam) = rest.remove("IPAM") {
            let ipam = json::object(&ipam, "IPAM")?;
            let kept = json::kept(ipam, &["Driver"], "IPAM")?;
            if !matches!(json::string(&kept, "Driver")?, "" | "default") {
                return Err("IPAM.Driver is not the default".into());
            }
        }
        let kept = json::kept(&rest, &["Name", "Driver", "Internal"], "network")?;
        let name = json::string(&kept, "Name")?;
        if !is_internal_network(name) {
            return Err(format!(
                "network {name:?} is not the fetch step's or a gate run's sidecars'"
            ));
        }
        if !json::bool(&kept, "Internal")? {
            return Err(format!("network {name:?} must be internal"));
        }
        let driver = json::string(&kept, "Driver")?;
        if !matches!(driver, "" | "bridge") {
            return Err(format!("network driver {driver:?}"));
        }
        Ok(json!({ "Name": name, "Driver": "bridge", "Internal": true }))
    }

    /// The body to send for `POST /networks/<network>/connect`: one of
    /// aegis's containers, with no settings of its own on the fetch
    /// step's network (no aliases to take another's name).
    pub fn connect(&self, network: &str, body: &Value) -> Result<Value, String> {
        self.check_network(network)?;
        let kept = json::kept(json::object(body, "connect")?, &["Container"], "connect")?;
        let container = json::string(&kept, "Container")?;
        if !is_aegis_container(container) {
            return Err(format!("container {container:?} is not one of aegis's"));
        }
        Ok(json!({ "Container": container }))
    }

    /// Checks `GET` and `DELETE /networks/<network>`.
    pub fn check_network(&self, network: &str) -> Result<(), String> {
        if is_internal_network(network) {
            Ok(())
        } else {
            Err(format!(
                "network {network:?} is not the fetch step's or a gate run's sidecars'"
            ))
        }
    }
}

#[cfg(test)]
mod tests;
