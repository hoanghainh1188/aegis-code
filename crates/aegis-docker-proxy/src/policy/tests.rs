use super::*;
use serde_json::json;

/// A workspace with a job tree, and a directory beside it.
struct Dirs {
    root: PathBuf,
    workspace: PathBuf,
    outside: PathBuf,
}

impl Dirs {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("aegis-proxy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        std::fs::create_dir_all(workspace.join("jobs/1/repo")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        Self {
            root,
            workspace,
            outside,
        }
    }

    fn policy(&self) -> Policy {
        Policy {
            mount_roots: vec![self.workspace.clone()],
            ..Policy::default()
        }
    }

    fn tree(&self) -> String {
        self.workspace.join("jobs/1/repo").display().to_string()
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// What the docker 29 client sends for a gate step (`Hardening::args`,
/// #320), recorded through a socket.
fn step(tree: &str) -> Value {
    let mut body: Value = serde_json::from_str(STEP).unwrap();
    body["HostConfig"]["Binds"] = json!([format!("{tree}:/work")]);
    body
}

const STEP: &str = r#"{"Hostname":"","Domainname":"","User":"501:20","AttachStdin":false,"AttachStdout":true,"AttachStderr":true,"Tty":false,"OpenStdin":false,"StdinOnce":false,"Env":["A=B","SONAR_TOKEN"],"Cmd":["cargo","test"],"Image":"img","Volumes":{},"WorkingDir":"/work","Entrypoint":null,"Labels":{},"HostConfig":{"Binds":[],"ContainerIDFile":"","LogConfig":{"Type":"","Config":{}},"NetworkMode":"none","PortBindings":{},"RestartPolicy":{"Name":"no","MaximumRetryCount":0},"AutoRemove":false,"VolumeDriver":"","VolumesFrom":null,"ConsoleSize":[0,0],"CapAdd":null,"CapDrop":["ALL"],"CgroupnsMode":"","Dns":null,"DnsOptions":[],"DnsSearch":[],"ExtraHosts":null,"GroupAdd":null,"IpcMode":"","Cgroup":"","Links":null,"OomScoreAdj":0,"PidMode":"","Privileged":false,"PublishAllPorts":false,"ReadonlyRootfs":true,"SecurityOpt":["no-new-privileges"],"Tmpfs":{"/tmp":"rw,exec,nosuid,nodev,size=1024m"},"UTSMode":"","UsernsMode":"","ShmSize":0,"Isolation":"","CpuShares":0,"Memory":8589934592,"NanoCpus":4000000000,"CgroupParent":"","BlkioWeight":0,"BlkioWeightDevice":[],"BlkioDeviceReadBps":[],"BlkioDeviceWriteBps":[],"BlkioDeviceReadIOps":[],"BlkioDeviceWriteIOps":[],"CpuPeriod":0,"CpuQuota":0,"CpuRealtimePeriod":0,"CpuRealtimeRuntime":0,"CpusetCpus":"","CpusetMems":"","Devices":[],"DeviceCgroupRules":null,"DeviceRequests":null,"MemoryReservation":0,"MemorySwap":8589934592,"MemorySwappiness":-1,"OomKillDisable":false,"PidsLimit":4096,"Ulimits":null,"CpuCount":0,"CpuPercent":0,"IOMaximumIOps":0,"IOMaximumBandwidth":0,"Mounts":[{"Type":"volume","Target":"/home/aegis"}],"MaskedPaths":null,"ReadonlyPaths":null},"NetworkingConfig":{"EndpointsConfig":{}}}"#;

fn refused(policy: &Policy, body: &Value) -> String {
    policy.create(body).unwrap_err()
}

#[test]
fn a_gate_step_is_rebuilt_from_what_was_checked() {
    let dirs = Dirs::new("step");
    let out = dirs.policy().create(&step(&dirs.tree())).unwrap();
    let host = &out["HostConfig"];
    assert_eq!(host["Binds"], json!([format!("{}:/work", dirs.tree())]));
    assert_eq!(host["CapDrop"], json!(["ALL"]));
    assert_eq!(host["Memory"], json!(8_589_934_592i64));
    assert_eq!(out["Env"], json!(["A=B", "SONAR_TOKEN"]));
    // Defaults left out: the daemon's own.
    for key in [
        "MaskedPaths",
        "LogConfig",
        "RestartPolicy",
        "Privileged",
        "CapAdd",
    ] {
        assert!(host.get(key).is_none(), "{key}");
    }
    assert_eq!(
        out["NetworkingConfig"],
        json!({ "EndpointsConfig": { "none": {} } })
    );
}

#[test]
fn a_key_in_another_case_or_repeated_cannot_hide_from_the_check() {
    // Go matches keys without regard to case; the last one wins.
    let dirs = Dirs::new("case");
    for (key, value) in [
        ("privileged", json!(true)),
        ("capdrop", json!(["NET_RAW"])),
        ("readonlyrootfs", json!(true)),
        ("networkmode", json!("host")),
        ("binds", json!(["/:/h"])),
        ("Capabilities", json!(["CAP_SYS_ADMIN"])),
        ("GroupAdd", json!(["0"])),
        ("MaskedPaths", json!(["/x"])),
        (
            "LogConfig",
            json!({ "Type": "gelf", "Config": { "gelf-address": "udp://x:1" } }),
        ),
        ("VolumeDriver", json!("local")),
        ("ShmSize", json!(1)),
    ] {
        let mut body = step(&dirs.tree());
        body["HostConfig"][key] = value;
        let reason = refused(&dirs.policy(), &body);
        assert!(reason.contains(key), "{key}: {reason}");
    }
    // Zero in another case is what the daemon assumes anyway: left out,
    // the checked value is sent.
    let mut body = step(&dirs.tree());
    body["HostConfig"]["capdrop"] = json!([]);
    body["HostConfig"]["readonlyrootfs"] = json!(false);
    let out = dirs.policy().create(&body).unwrap();
    assert_eq!(out["HostConfig"]["CapDrop"], json!(["ALL"]));
    assert_eq!(out["HostConfig"]["ReadonlyRootfs"], json!(true));
    assert!(out["HostConfig"].get("capdrop").is_none());
    let mut body = step(&dirs.tree());
    body["hostconfig"] = json!({ "Privileged": true });
    assert!(refused(&dirs.policy(), &body).contains("hostconfig"));
    let mut body = step(&dirs.tree());
    body["user"] = json!("0");
    assert!(refused(&dirs.policy(), &body).contains("user"));
}

#[test]
fn mounting_the_machine_is_refused() {
    let dirs = Dirs::new("root");
    let mut body = step(&dirs.tree());
    body["HostConfig"]["Binds"] = json!(["/:/host"]);
    assert!(refused(&dirs.policy(), &body).contains("outside the workspace"));
    body["HostConfig"]["Binds"] = json!([format!("{}:/x", dirs.outside.display())]);
    assert!(refused(&dirs.policy(), &body).contains("outside the workspace"));
    let mut body = step(&dirs.tree());
    body["HostConfig"]["Mounts"] = json!([{ "Type": "bind", "Source": "/", "Target": "/host" }]);
    assert!(refused(&dirs.policy(), &body).contains("outside the workspace"));
}

#[test]
fn a_volume_with_driver_options_is_refused() {
    // The `local` driver can bind any host path.
    let dirs = Dirs::new("volume");
    let mut body = step(&dirs.tree());
    body["HostConfig"]["Mounts"] = json!([{
        "Type": "volume",
        "Target": "/h",
        "VolumeOptions": { "DriverConfig": { "Name": "local", "Options": { "type": "none", "o": "bind", "device": "/" } } },
    }]);
    assert!(refused(&dirs.policy(), &body).contains("VolumeOptions"));
    body["HostConfig"]["Mounts"] = json!([{ "Type": "bind", "Source": dirs.tree(), "Target": "/w", "BindOptions": { "Propagation": "rshared" } }]);
    assert!(refused(&dirs.policy(), &body).contains("BindOptions"));
}

#[test]
fn a_link_out_of_the_workspace_is_refused() {
    let dirs = Dirs::new("link");
    let link = dirs.workspace.join("jobs/1/escape");
    std::os::unix::fs::symlink(&dirs.outside, &link).unwrap();
    let mut body = step(&dirs.tree());
    body["HostConfig"]["Binds"] = json!([format!("{}:/x", link.display())]);
    assert!(refused(&dirs.policy(), &body).contains("outside the workspace"));
}

#[test]
fn relative_parent_missing_and_named_sources_are_refused() {
    let dirs = Dirs::new("paths");
    let up = format!("{}/../outside", dirs.workspace.display());
    let missing = dirs.workspace.join("nothing-here").display().to_string();
    for (source, why) in [
        (up.as_str(), "plain absolute path"),
        ("jobs", "plain absolute path"),
        ("some-volume", "plain absolute path"),
        (missing.as_str(), "nothing-here"),
    ] {
        let mut body = step(&dirs.tree());
        body["HostConfig"]["Binds"] = json!([format!("{source}:/x")]);
        assert!(refused(&dirs.policy(), &body).contains(why), "{source}");
    }
    let mut body = step(&dirs.tree());
    body["HostConfig"]["Binds"] = json!([format!("{}:/x:rshared", dirs.tree())]);
    assert!(refused(&dirs.policy(), &body).contains("options"));
    let mut body = step(&dirs.tree());
    body["HostConfig"]["Mounts"] =
        json!([{ "Type": "volume", "Source": "shared", "Target": "/x" }]);
    assert!(refused(&dirs.policy(), &body).contains("mount of type"));
}

#[test]
fn escapes_from_the_sandbox_are_refused() {
    let dirs = Dirs::new("escapes");
    for (key, value) in [
        ("Privileged", json!(true)),
        ("CapAdd", json!(["SYS_ADMIN"])),
        ("Devices", json!([{ "PathOnHost": "/dev/sda" }])),
        ("PidMode", json!("host")),
        ("IpcMode", json!("host")),
        ("UsernsMode", json!("host")),
        ("VolumesFrom", json!(["aegis"])),
        ("Sysctls", json!({ "kernel.x": "1" })),
        ("PortBindings", json!({ "80/tcp": [{ "HostPort": "80" }] })),
        ("CgroupParent", json!("/")),
        ("OomKillDisable", json!(true)),
        ("RestartPolicy", json!({ "Name": "always" })),
    ] {
        let mut body = step(&dirs.tree());
        body["HostConfig"][key] = value;
        let reason = refused(&dirs.policy(), &body);
        assert!(reason.contains(key), "{key}: {reason}");
    }
    let mut body = step(&dirs.tree());
    body["HostConfig"]["NetworkMode"] = json!("host");
    assert!(refused(&dirs.policy(), &body).contains("network"));
    let mut body = step(&dirs.tree());
    body["HostConfig"]["Runtime"] = json!("nvidia");
    assert!(refused(&dirs.policy(), &body).contains("runtime"));
}

#[test]
fn the_hardening_of_320_is_required_exactly() {
    let dirs = Dirs::new("hardening");
    for (key, value, why) in [
        ("CapDrop", json!([]), "CapDrop"),
        ("CapDrop", json!(["ALL", "x"]), "CapDrop"),
        ("SecurityOpt", json!([]), "SecurityOpt"),
        (
            "SecurityOpt",
            json!([
                "no-new-privileges",
                "seccomp={\"defaultAction\":\"SCMP_ACT_ALLOW\"}"
            ]),
            "SecurityOpt",
        ),
        (
            "SecurityOpt",
            json!(["no-new-privileges", "apparmor=x"]),
            "SecurityOpt",
        ),
        ("Memory", json!(0), "Memory"),
        ("NanoCpus", json!(0), "NanoCpus"),
        ("PidsLimit", json!(-1), "PidsLimit"),
        ("MemorySwap", json!(-1), "MemorySwap"),
        ("ReadonlyRootfs", json!(false), "read-only root"),
    ] {
        let mut body = step(&dirs.tree());
        body["HostConfig"][key] = value.clone();
        let reason = refused(&dirs.policy(), &body);
        assert!(reason.contains(why), "{key} {value}: {reason}");
    }
}

#[test]
fn the_user_is_a_plain_non_root_uid_and_gid() {
    let dirs = Dirs::new("user");
    for user in [
        "",
        "0",
        "root",
        "0:0",
        "00:20",
        "+0:20",
        "-0:20",
        "501",
        "501:0",
        "aegis:aegis",
        "501:20:1",
    ] {
        let mut body = step(&dirs.tree());
        body["User"] = json!(user);
        assert!(
            refused(&dirs.policy(), &body).contains("non-root"),
            "{user:?}"
        );
    }
    let mut body = step(&dirs.tree());
    body["User"] = json!("10001:10001");
    dirs.policy().create(&body).unwrap();
}

#[test]
fn a_tmpfs_has_no_set_uid_files_or_devices() {
    let dirs = Dirs::new("tmpfs");
    for opts in [
        "rw,size=1m",
        "rw,nosuid,size=1m",
        "rw,nosuid,nodev,suid",
        "nodev,nosuid,dev",
    ] {
        let mut body = step(&dirs.tree());
        body["HostConfig"]["Tmpfs"] = json!({ "/tmp": opts });
        assert!(refused(&dirs.policy(), &body).contains("tmpfs"), "{opts}");
    }
}

#[test]
fn a_writable_root_is_for_the_listed_images_only() {
    let dirs = Dirs::new("writable");
    let mut body = step(&dirs.tree());
    body["Image"] = json!("sonarsource/sonar-scanner-cli");
    body["HostConfig"]["ReadonlyRootfs"] = json!(false);
    assert!(refused(&dirs.policy(), &body).contains("read-only root"));
    let policy = Policy {
        writable_images: vec!["sonarsource/sonar-scanner-cli".into()],
        ..dirs.policy()
    };
    policy.create(&body).unwrap();
}

#[test]
fn networks_are_none_the_fetch_steps_or_listed() {
    let dirs = Dirs::new("networks");
    let with = |net: &str| {
        let mut body = step(&dirs.tree());
        body["HostConfig"]["NetworkMode"] = json!(net);
        body
    };
    dirs.policy().create(&with("aegis-fetch-42-0")).unwrap();
    // A job's tree on a network that reaches out: refused unless listed.
    for net in ["bridge", "sonar", "aegis-fetch-", "container:aegis"] {
        assert!(
            refused(&dirs.policy(), &with(net)).contains("network"),
            "{net}"
        );
    }
    let listed = Policy {
        networks: vec!["sonar".into()],
        ..dirs.policy()
    };
    listed.create(&with("sonar")).unwrap();
    // No second endpoint, and none with settings.
    let mut body = with("aegis-fetch-42-0");
    body["NetworkingConfig"] = json!({ "EndpointsConfig": { "bridge": {} } });
    assert!(refused(&dirs.policy(), &body).contains("endpoint"));
    body["NetworkingConfig"] =
        json!({ "EndpointsConfig": { "aegis-fetch-42-0": { "Aliases": ["pypi.org"] } } });
    assert!(refused(&dirs.policy(), &body).contains("endpoint"));
}

#[test]
fn the_egress_proxy_is_on_the_bridge_with_nothing_mounted() {
    let dirs = Dirs::new("egress");
    let mut body = step(&dirs.tree());
    body["HostConfig"]["Binds"] = json!(null);
    body["HostConfig"]["Mounts"] = json!(null);
    body["HostConfig"]["NetworkMode"] = json!("bridge");
    let out = dirs.policy().create(&body).unwrap();
    assert_eq!(out["HostConfig"]["NetworkMode"], "bridge");
}

#[test]
fn only_the_calls_aegis_makes_are_routed() {
    let c = |id: &str| Route::Container(id.into());
    for (method, path, query, expected) in [
        ("HEAD", "/_ping", None, Route::Handshake),
        ("GET", "/version", None, Route::Handshake),
        ("GET", "/v1.56/version", None, Route::Handshake),
        (
            "POST",
            "/v1.56/containers/create",
            Some("name=aegis-gate-1-0"),
            Route::CreateContainer("aegis-gate-1-0".into()),
        ),
        (
            "POST",
            "/v1.56/containers/abc/attach",
            Some("stream=1&stdout=1&stderr=1"),
            Route::Attach("abc".into()),
        ),
        (
            "POST",
            "/v1.56/containers/abc/wait",
            Some("condition=next-exit"),
            c("abc"),
        ),
        ("POST", "/v1.56/containers/abc/start", None, c("abc")),
        ("POST", "/v1.56/containers/abc/kill", None, c("abc")),
        (
            "GET",
            "/v1.56/containers/aegis-gate-1-0/json",
            None,
            c("aegis-gate-1-0"),
        ),
        (
            "GET",
            "/v1.56/containers/abc/logs",
            Some("follow=1&stdout=1&stderr=1"),
            c("abc"),
        ),
        (
            "DELETE",
            "/v1.56/containers/abc",
            Some("force=1&v=1"),
            c("abc"),
        ),
        ("POST", "/v1.56/networks/create", None, Route::CreateNetwork),
        (
            "POST",
            "/v1.56/networks/aegis-fetch-1-0/connect",
            None,
            Route::ConnectNetwork("aegis-fetch-1-0".into()),
        ),
        (
            "DELETE",
            "/v1.56/networks/aegis-fetch-1-0",
            None,
            Route::Network("aegis-fetch-1-0".into()),
        ),
        (
            "GET",
            "/v1.56/networks/aegis-fetch-1-0",
            None,
            Route::Network("aegis-fetch-1-0".into()),
        ),
        (
            "POST",
            "/v1.56/images/create",
            Some("fromImage=docker.io%2Flibrary%2Fimg&tag=latest"),
            Route::Image,
        ),
        (
            "GET",
            "/v1.56/images/ghcr.io/x/aegis-sandbox-rust/json",
            None,
            Route::Image,
        ),
    ] {
        assert_eq!(route(method, path, query), Ok(expected), "{method} {path}");
    }
    for (method, path, query) in [
        ("POST", "/v1.56/containers/abc/exec", None),
        ("POST", "/v1.56/exec/abc/start", None),
        ("POST", "/v1.56/build", None),
        ("GET", "/v1.56/containers/json", None),
        ("PUT", "/v1.56/containers/abc/archive", None),
        ("POST", "/v1.56/containers/abc/update", None),
        ("POST", "/v1.56/volumes/create", None),
        ("GET", "/v1.56/plugins", None),
        ("POST", "/v1.56/swarm/init", None),
        ("POST", "/v1.56/networks/aegis-fetch-1-0/disconnect", None),
        ("POST", "/v1.56/containers/a%2Fb/start", None),
        ("GET", "/v1.56/images/../containers/postgres/json", None),
        ("GET", "/v1.56/images/%2e%2e/containers/postgres/json", None),
        ("GET", "/v1.56/images/x/./json", None),
        (
            "POST",
            "/v1.56//containers/create",
            Some("name=aegis-gate-1-0"),
        ),
        // Old APIs, and versions the daemon would read otherwise.
        ("POST", "/v1.23/containers/abc/start", None),
        ("POST", "/v1.40/containers/abc/start", None),
        ("GET", "/v/version", None),
        ("GET", "/v1.2.3/version", None),
        // Names and queries.
        ("POST", "/v1.56/containers/create", None),
        ("POST", "/v1.56/containers/create", Some("name=postgres")),
        (
            "POST",
            "/v1.56/containers/create",
            Some("n%61me=evil&name=aegis-gate-1-0"),
        ),
        ("POST", "/v1.56/images/create", Some("fromSrc=-")),
        (
            "POST",
            "/v1.56/images/create",
            Some("fromImage=x&fromSrc=http://169.254.169.254/"),
        ),
        ("POST", "/v1.56/containers/abc/start", Some("detachKeys=x")),
    ] {
        assert!(
            route(method, path, query).is_err(),
            "{method} {path} {query:?}"
        );
    }
}

#[test]
fn a_container_needs_one_of_aegiss_names() {
    assert!(is_aegis_container("/aegis-lsp-7-3"));
    for name in [
        "aegis",
        "aegis-gate-",
        "/aegis-gate-1/../x",
        "postgres",
        "aegis-docker-proxy",
    ] {
        assert!(!is_aegis_container(name), "{name}");
    }
}

#[test]
fn networks_are_the_fetch_steps_internal_ones() {
    let policy = Policy::default();
    // What the docker 29 client sends for `network create --internal`.
    let sent = json!({"Name":"aegis-fetch-1-0","Driver":"bridge","Scope":"","IPAM":{"Driver":"default","Options":{},"Config":[]},"Internal":true,"Attachable":false,"Ingress":false,"ConfigOnly":false,"ConfigFrom":null,"Options":{},"Labels":{}});
    let rebuilt = json!({ "Name": "aegis-fetch-1-0", "Driver": "bridge", "Internal": true });
    assert_eq!(policy.network_create(&sent).unwrap(), rebuilt);
    let mut body = sent.clone();
    body["internal"] = json!(false);
    assert_eq!(policy.network_create(&body).unwrap(), rebuilt);
    for (key, value) in [
        ("Internal", json!(false)),
        ("driver", json!("macvlan")),
        ("Name", json!("open")),
        ("Driver", json!("macvlan")),
        (
            "Options",
            json!({ "com.docker.network.bridge.name": "docker0" }),
        ),
        ("ConfigFrom", json!({ "Network": "x" })),
        ("IPAM", json!({ "Driver": "custom" })),
    ] {
        let mut body = sent.clone();
        body[key] = value;
        assert!(policy.network_create(&body).is_err(), "{key}");
    }
    let connect = json!({"Container":"aegis-egress-1-0","EndpointConfig":{"IPAMConfig":{},"Links":null,"Aliases":[],"DriverOpts":{},"GwPriority":0,"NetworkID":"","EndpointID":"","Gateway":"","IPAddress":"","MacAddress":"","IPPrefixLen":0,"IPv6Gateway":"","GlobalIPv6Address":"","GlobalIPv6PrefixLen":0,"DNSNames":null}});
    assert_eq!(
        policy.connect("aegis-fetch-1-0", &connect).unwrap(),
        json!({ "Container": "aegis-egress-1-0" })
    );
    assert!(policy.connect("bridge", &connect).is_err());
    for (path, value) in [
        ("/Container", json!("postgres")),
        ("/EndpointConfig/Aliases", json!(["pypi.org"])),
    ] {
        let mut body = connect.clone();
        *body.pointer_mut(path).unwrap() = value;
        assert!(policy.connect("aegis-fetch-1-0", &body).is_err(), "{path}");
    }
    let mut body = connect.clone();
    body["container"] = json!("postgres");
    assert!(policy.connect("aegis-fetch-1-0", &body).is_err());
    policy.check_network("aegis-fetch-1-0").unwrap();
    assert!(policy.check_network("bridge").is_err());
}
