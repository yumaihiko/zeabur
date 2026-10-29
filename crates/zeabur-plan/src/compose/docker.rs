//! Running Docker containers, read from `docker inspect`, as the services Zeabur makes from them.
//!
//! Each container becomes the Compose service it would be [inspect], and the Compose import
//! reads the result. The paste can also hold the containers' images, since `docker inspect`
//! takes image IDs too. Then the variables, command, arguments and health check that a container
//! only inherits from its image are left out, so the image's next version sets them.
//!
//! - **Name.** A container that Compose started keeps its Compose service name
//!   (`com.docker.compose.service`); others keep their container name. Containers of one Compose
//!   service are one service with as many replicas.
//! - **Image.** `Config.Image`, as the container was started. An image ID is not made, and an
//!   image that no registry holds (no `RepoDigests`) is listed in [`Finding::LocalImage`].
//! - **Dependencies.** `com.docker.compose.depends_on`, as Compose writes it.
//!
//! [inspect]: https://docs.docker.com/reference/api/engine/version/v1.47/#tag/Container/operation/ContainerInspect

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{ComposeRequest, Conversion, Finding, MAX_SERVICES, Reader, read_services, strings};
use crate::plan::{ErrorKind, PlanError, error};

/// The longest paste read, in bytes: `docker inspect` writes about 10 KB for each container.
const MAX_JSON: usize = 1024 * 1024;
/// The `PATH` Docker gives a container whose image sets none.
const DOCKER_PATH: &str = "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const SERVICE_LABEL: &str = "com.docker.compose.service";
const DEPENDS_LABEL: &str = "com.docker.compose.depends_on";

/// The output of `docker inspect` to convert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct DockerInspectRequest {
    /// The JSON that `docker inspect` writes: containers, and optionally their images.
    pub json: String,
}

/// Reads `request`; `wait_image` is the image that waits for a dependency.
///
/// # Errors
///
/// When the JSON does not read, is too large, or holds no containers or too many.
pub fn convert(request: &DockerInspectRequest, wait_image: &str) -> Result<Conversion, PlanError> {
    if request.json.len() > MAX_JSON {
        return Err(error(
            ErrorKind::BadRequest,
            "too_large",
            "The output of docker inspect is larger than 1 MB.",
        ));
    }
    let root: Value = serde_json::from_str(&request.json).map_err(|e| {
        let mut refused = error(
            ErrorKind::BadRequest,
            "invalid_json",
            "The JSON does not read: paste the whole output of docker inspect.",
        );
        refused.line = u64::try_from(e.line()).ok();
        refused
    })?;
    let objects = match root {
        Value::Array(items) => items,
        object @ Value::Object(_) => vec![object],
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Vec::new(),
    };
    let images: BTreeMap<&str, &Value> = objects
        .iter()
        .filter(|o| o.get("RootFS").is_some())
        .filter_map(|o| Some((o.get("Id")?.as_str()?, o)))
        .collect();
    let containers: Vec<&Value> = objects
        .iter()
        .filter(|o| o.get("HostConfig").is_some())
        .collect();
    if containers.is_empty() {
        return Err(error(
            ErrorKind::BadRequest,
            "no_containers",
            "The JSON holds no containers: run docker inspect with the names of running containers.",
        ));
    }
    let mut services = Map::new();
    let mut findings: BTreeMap<String, Vec<Finding>> = BTreeMap::new();
    for container in containers {
        let name = name_of(container);
        if let Some(Value::Object(spec)) = services.get_mut(&name) {
            let scale = spec.get("scale").and_then(Value::as_u64).unwrap_or(1);
            spec.insert("scale".to_owned(), json!(scale + 1));
            continue;
        }
        let image = container
            .get("Image")
            .and_then(Value::as_str)
            .and_then(|id| images.get(id).copied());
        let (spec, found) = spec_of(container, &name, image);
        services.insert(name.clone(), spec);
        findings.insert(name, found);
    }
    if services.len() > MAX_SERVICES {
        return Err(error(
            ErrorKind::BadRequest,
            "service_count",
            "Import at most 50 containers at a time.",
        ));
    }
    let reader = Reader {
        dotenv: BTreeMap::new(),
        unset: BTreeSet::new(),
        literal: true,
    };
    let request = ComposeRequest {
        yaml: String::new(),
        env_files: BTreeMap::new(),
    };
    let mut conversion = read_services(&services, &request, reader, wait_image);
    for service in &mut conversion.services {
        if let Some(found) = findings.remove(&service.compose_name) {
            service.findings.extend(found);
        }
    }
    Ok(conversion)
}

/// The Compose service name, or the container name without its leading `/`.
fn name_of(container: &Value) -> String {
    let label = container
        .pointer("/Config/Labels")
        .and_then(|labels| labels.get(SERVICE_LABEL))
        .and_then(Value::as_str);
    let name = container.get("Name").and_then(Value::as_str);
    label
        .or(name.map(|n| n.trim_start_matches('/')))
        .filter(|n| !n.is_empty())
        .unwrap_or("app")
        .to_owned()
}

/// The container as a Compose service, and what the Compose import cannot see.
fn spec_of(container: &Value, name: &str, image: Option<&Value>) -> (Value, Vec<Finding>) {
    let mut findings = Vec::new();
    let mut spec = Map::new();
    let config = |key: &str| container.pointer(&format!("/Config/{key}"));
    let host = |key: &str| container.pointer(&format!("/HostConfig/{key}"));
    let own = |key: &str| image.and_then(|i| i.pointer(&format!("/Config/{key}")));

    let named = config("Image").and_then(Value::as_str).unwrap_or_default();
    if named.is_empty() || is_image_id(named) {
        findings.push(Finding::LocalImage {
            image: named.to_owned(),
        });
    } else {
        spec.insert("image".to_owned(), json!(named));
        match image {
            Some(image) if strings(image.get("RepoDigests")).is_empty() => {
                findings.push(Finding::LocalImage {
                    image: named.to_owned(),
                });
            }
            Some(_) => {}
            None => findings.push(Finding::ImageDefaults {
                image: named.to_owned(),
            }),
        }
    }
    if let Some(container_name) = container.get("Name").and_then(Value::as_str) {
        let container_name = container_name.trim_start_matches('/');
        if container_name != name {
            spec.insert("container_name".to_owned(), json!(container_name));
        }
    }

    let entrypoint = strings(config("Entrypoint"));
    let cmd = strings(config("Cmd"));
    let same_entrypoint = image.is_some() && entrypoint == strings(own("Entrypoint"));
    if !same_entrypoint && !entrypoint.is_empty() {
        spec.insert("entrypoint".to_owned(), json!(entrypoint));
    }
    if !(same_entrypoint && cmd == strings(own("Cmd"))) && !cmd.is_empty() {
        spec.insert("command".to_owned(), json!(cmd));
    }

    let inherited = strings(own("Env"));
    let environment: Vec<String> = strings(config("Env"))
        .into_iter()
        .filter(|pair| !inherited.contains(pair) && pair != DOCKER_PATH)
        .collect();
    spec.insert("environment".to_owned(), json!(environment));

    let healthcheck = config("Healthcheck").filter(|h| !h.is_null());
    if healthcheck.is_some() && healthcheck != own("Healthcheck").filter(|h| !h.is_null()) {
        spec.insert("healthcheck".to_owned(), json!({}));
    }

    let (ports, expose) = ports_of(config("ExposedPorts"), host("PortBindings"));
    spec.insert("ports".to_owned(), json!(ports));
    spec.insert("expose".to_owned(), json!(expose));
    spec.insert(
        "volumes".to_owned(),
        json!(volumes_of(container.get("Mounts"))),
    );

    if host("Privileged").and_then(Value::as_bool) == Some(true) {
        spec.insert("privileged".to_owned(), json!(true));
    }
    if host("NetworkMode").and_then(Value::as_str) == Some("host") {
        spec.insert("network_mode".to_owned(), json!("host"));
    }
    if host("PidMode").and_then(Value::as_str) == Some("host") {
        spec.insert("pid".to_owned(), json!("host"));
    }
    if let Some(capabilities) = host("CapAdd").filter(|c| !c.is_null()) {
        spec.insert("cap_add".to_owned(), capabilities.clone());
    }
    if let Some(devices) = host("Devices").filter(|d| !d.is_null()) {
        spec.insert("devices".to_owned(), devices.clone());
    }

    let depends_on: Vec<&str> = config("Labels")
        .and_then(|labels| labels.get(DEPENDS_LABEL))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| entry.split(':').next())
        .filter(|service| !service.is_empty())
        .collect();
    spec.insert("depends_on".to_owned(), json!(depends_on));
    (Value::Object(spec), findings)
}

/// `sha256:…`, or a hexadecimal ID of at least 12 digits, as `docker run <id>` records it.
fn is_image_id(image: &str) -> bool {
    let hex = image.strip_prefix("sha256:").unwrap_or(image);
    image.starts_with("sha256:")
        || (hex.len() >= 12 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The ports that Docker publishes on the host, as Compose `ports`, and the others as `expose`.
fn ports_of(exposed: Option<&Value>, bindings: Option<&Value>) -> (Vec<Value>, Vec<String>) {
    let bound = bindings.and_then(Value::as_object);
    let mut keys: Vec<&String> = exposed
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(Map::keys)
        .chain(bound.into_iter().flat_map(Map::keys))
        .collect();
    keys.sort_by_key(|key| {
        let (port, _) = key.split_once('/').unwrap_or((key.as_str(), "tcp"));
        (port.parse::<u16>().unwrap_or(u16::MAX), (*key).clone())
    });
    keys.dedup();
    let mut ports = Vec::new();
    let mut expose = Vec::new();
    for key in keys {
        let (port, protocol) = key.split_once('/').unwrap_or((key.as_str(), "tcp"));
        let published = bound
            .and_then(|b| b.get(key))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|b| b.get("HostPort").and_then(Value::as_str))
            .filter(|p| !p.is_empty());
        match published {
            Some(host_port) => ports.push(json!({
                "target": port,
                "published": host_port,
                "protocol": protocol,
            })),
            None => expose.push(format!("{port}/{protocol}")),
        }
    }
    (ports, expose)
}

/// `Mounts` as Compose long-syntax volumes. An anonymous volume has a generated name, so it gets
/// none.
fn volumes_of(mounts: Option<&Value>) -> Vec<Value> {
    mounts
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|mount| {
            let text = |key: &str| mount.get(key).and_then(Value::as_str).unwrap_or_default();
            let read_only = mount.get("RW").and_then(Value::as_bool) == Some(false);
            let (kind, source) = match text("Type") {
                "volume" if is_anonymous(text("Name")) => ("volume", ""),
                "volume" => ("volume", text("Name")),
                "bind" => ("bind", text("Source")),
                "tmpfs" => ("tmpfs", ""),
                _ => return None,
            };
            Some(json!({
                "type": kind,
                "source": source,
                "target": text("Destination"),
                "read_only": read_only,
            }))
        })
        .collect()
}

fn is_anonymous(volume: &str) -> bool {
    volume.len() == 64 && volume.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{ComposeService, Counterpart, Part};

    const WAIT: &str = "busybox:1.37.0";
    const PG_IMAGE: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const WEB_IMAGE: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const PG_DEFAULT_PATH: &str =
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/lib/postgresql/16/bin";

    fn read(objects: &Value) -> Conversion {
        let request = DockerInspectRequest {
            json: objects.to_string(),
        };
        convert(&request, WAIT).unwrap()
    }

    fn find<'a>(c: &'a Conversion, name: &str) -> &'a ComposeService {
        c.services.iter().find(|s| s.name == name).unwrap()
    }

    fn postgres_image() -> Value {
        json!({
            "Id": PG_IMAGE,
            "RepoTags": ["postgres:16"],
            "RepoDigests": ["postgres@sha256:abcd"],
            "RootFS": {"Type": "layers"},
            "Config": {
                "Env": [PG_DEFAULT_PATH, "PG_MAJOR=16", "PGDATA=/var/lib/postgresql/data"],
                "Entrypoint": ["docker-entrypoint.sh"],
                "Cmd": ["postgres"],
                "ExposedPorts": {"5432/tcp": {}},
            },
        })
    }

    fn postgres_container() -> Value {
        json!({
            "Name": "/shop-db-1",
            "Image": PG_IMAGE,
            "Config": {
                "Image": "postgres:16",
                "Env": ["POSTGRES_PASSWORD=s3cret", "POSTGRES_DB=shop", PG_DEFAULT_PATH, "PG_MAJOR=16",
                        "PGDATA=/var/lib/postgresql/data"],
                "Entrypoint": ["docker-entrypoint.sh"],
                "Cmd": ["postgres"],
                "ExposedPorts": {"5432/tcp": {}},
                "Labels": {"com.docker.compose.service": "db"},
            },
            "HostConfig": {"NetworkMode": "shop_default", "PortBindings": {}},
            "Mounts": [
                {"Type": "volume", "Name": "shop_pgdata", "Destination": "/var/lib/postgresql/data", "RW": true},
            ],
        })
    }

    fn web_container(name: &str) -> Value {
        json!({
            "Name": name,
            "Image": WEB_IMAGE,
            "Config": {
                "Image": "ghcr.io/acme/web:1.4",
                "Env": ["DATABASE_URL=postgres://postgres:s3cret@db:5432/shop", "PRICE=$5", DOCKER_PATH],
                "Entrypoint": null,
                "Cmd": ["node", "server.js"],
                "ExposedPorts": {"3000/tcp": {}, "9229/tcp": {}},
                "Labels": {
                    "com.docker.compose.service": "web",
                    "com.docker.compose.depends_on": "db:service_started:false",
                },
            },
            "HostConfig": {
                "PortBindings": {"3000/tcp": [{"HostIp": "", "HostPort": "8080"}]},
                "CapAdd": null,
                "Devices": [],
            },
            "Mounts": [
                {"Type": "bind", "Source": "/var/run/docker.sock", "Destination": "/var/run/docker.sock", "RW": false},
                {"Type": "volume", "Name": "a".repeat(64), "Destination": "/tmp/cache", "RW": true},
            ],
        })
    }

    #[test]
    fn compose_containers_with_their_images() {
        let c = read(&json!([
            web_container("/shop-web-1"),
            web_container("/shop-web-2"),
            postgres_container(),
            postgres_image(),
        ]));
        assert_eq!(c.wait_image, WAIT);
        assert_eq!(c.services.len(), 2);

        let db = find(&c, "db");
        assert_eq!(db.image.as_deref(), Some("postgres:16"));
        assert_eq!(db.counterpart, Some(Counterpart::Postgresql));
        assert_eq!(db.command, Vec::<String>::new());
        assert_eq!(db.args, Vec::<String>::new());
        let names: Vec<&str> = db.variables.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["POSTGRES_PASSWORD", "POSTGRES_DB"]);
        assert_eq!(db.ports, [5432]);
        assert!(!db.published);
        assert_eq!(db.volumes.len(), 1);
        assert_eq!(db.volumes[0].mount_path, "/var/lib/postgresql/data");
        assert_eq!(db.findings, Vec::<Finding>::new());

        let web = find(&c, "web");
        assert_eq!(web.replicas, 2);
        assert_eq!(web.args, ["node", "server.js"]);
        assert_eq!(web.ports, [3000, 9229]);
        assert!(web.published);
        assert_eq!(web.depends_on, ["db"]);
        let price = web.variables.iter().find(|v| v.name == "PRICE").unwrap();
        assert_eq!(price.value, "$5");
        assert!(web.variables.iter().all(|v| v.name != "PATH"));
        let url = web.variables.iter().find(|v| v.name == "DATABASE_URL").unwrap();
        assert_eq!(url.refers.as_ref().map(|r| (r.service.as_str(), r.part)), Some(("db", Part::Url)));
        assert_eq!(web.volumes.len(), 1);
        assert_eq!(web.volumes[0].mount_path, "/tmp/cache");
        assert_eq!(
            web.findings,
            [
                Finding::DockerSocket {
                    source: "/var/run/docker.sock".to_owned()
                },
                Finding::ImageDefaults {
                    image: "ghcr.io/acme/web:1.4".to_owned()
                },
            ]
        );
    }

    #[test]
    fn container_without_its_image_keeps_everything() {
        let c = read(&postgres_container());
        let db = find(&c, "db");
        assert_eq!(db.command, ["docker-entrypoint.sh"]);
        assert_eq!(db.args, ["postgres"]);
        assert!(db.variables.iter().any(|v| v.name == "PG_MAJOR"));
        assert_eq!(
            db.findings,
            [Finding::ImageDefaults {
                image: "postgres:16".to_owned()
            }]
        );
    }

    #[test]
    fn local_images_and_host_rights_are_listed() {
        let mut built = web_container("/api");
        built["Config"]["Labels"] = json!({});
        built["Config"]["Image"] = json!("acme-api");
        let mut image = postgres_image();
        image["Id"] = json!(WEB_IMAGE);
        image["RepoDigests"] = json!([]);
        let by_id = json!({
            "Name": "/Worker_1",
            "Image": "sha256:3333",
            "Config": {"Image": "3f4e5d6c7b8a", "Env": [], "Cmd": ["run"]},
            "HostConfig": {"Privileged": true, "NetworkMode": "host", "PidMode": "host",
                           "CapAdd": ["NET_ADMIN"], "Devices": [{"PathOnHost": "/dev/fuse"}]},
            "Mounts": [{"Type": "bind", "Source": "/srv/data", "Destination": "/data", "RW": true}],
        });
        let c = read(&json!([built, image, by_id]));

        let api = find(&c, "api");
        assert_eq!(api.image.as_deref(), Some("acme-api"));
        assert!(api.findings.contains(&Finding::LocalImage {
            image: "acme-api".to_owned()
        }));
        assert_eq!(api.depends_on, Vec::<String>::new());

        let worker = find(&c, "worker-1");
        assert_eq!(worker.image, None);
        assert_eq!(
            worker.findings,
            [
                Finding::Renamed,
                Finding::Privileged,
                Finding::HostNetwork,
                Finding::HostPid,
                Finding::CapAdd {
                    capabilities: vec!["NET_ADMIN".to_owned()]
                },
                Finding::Devices,
                Finding::HostPath {
                    source: "/srv/data".to_owned(),
                    target: "/data".to_owned()
                },
                Finding::LocalImage {
                    image: "3f4e5d6c7b8a".to_owned()
                },
            ]
        );
    }

    #[test]
    fn bad_input_is_refused_with_a_code() {
        let image = postgres_image().to_string();
        let cases = [
            ("not json", "{\"Name\": ", "invalid_json"),
            ("no containers", "[]", "no_containers"),
            ("only an image", image.as_str(), "no_containers"),
            ("a string", "\"web\"", "no_containers"),
        ];
        for (case, json, code) in cases {
            let request = DockerInspectRequest {
                json: json.to_owned(),
            };
            let refused = convert(&request, WAIT).unwrap_err();
            assert_eq!(refused.code, code, "{case}");
        }
        let too_many: Vec<Value> = (0..51)
            .map(|n| web_container(&format!("/w{n}")))
            .map(|mut c| {
                c["Config"]["Labels"] = json!({});
                c
            })
            .collect();
        let refused = convert(
            &DockerInspectRequest {
                json: Value::Array(too_many).to_string(),
            },
            WAIT,
        )
        .unwrap_err();
        assert_eq!(refused.code, "service_count");
    }
}
