//! Removing components that left the configuration ([`zeabur_installer_plan::remove`]): what the
//! installer installed and what still uses it, read from the cluster, and the deletes.

use std::collections::BTreeSet;

use serde_json::Value;
use zeabur_installer_host::kubectl::Kubectl;
use zeabur_installer_plan::catalog::Index;
use zeabur_installer_plan::plan::{ComponentPlan, Warning};
use zeabur_installer_plan::remove::{self, InUse, Installed, Removal};

use crate::{InstallError, out};

/// The `HelmChart` resource.
const HELM_CHARTS: &str = "helmcharts.helm.cattle.io";
/// The most objects a refusal names for one component.
const MAX_NAMED: usize = 10;

/// What is planned for removal, with what uses each component. Empty when `kubectl` is `None`,
/// as before the cluster runs.
pub(crate) fn plan(
    kubectl: Option<&Kubectl>,
    index: &Index,
    planned: &[ComponentPlan],
    profile: &str,
    allow_data_loss: &[String],
    warnings: &mut Vec<Warning>,
) -> Result<Vec<Removal>, InstallError> {
    let Some(kubectl) = kubectl else {
        return Ok(Vec::new());
    };
    let installed = installed(kubectl)?;
    let (mut removals, more) =
        remove::removals(index, planned, &installed, profile, allow_data_loss);
    warnings.extend(more);
    if removals.is_empty() {
        return Ok(removals);
    }
    let crds = list_cluster(kubectl, "customresourcedefinitions")?;
    for removal in &mut removals {
        removal.delete_crds(remove::release_crds(&crds, &installed, &removal.id));
    }
    let served: BTreeSet<&str> = crds.iter().map(name).collect();
    let together: BTreeSet<String> = removals
        .iter()
        .flat_map(|r| r.namespaces.iter().cloned())
        .collect();
    for removal in &mut removals {
        removal.used_by = used_by(kubectl, removal, &together, &served)?;
    }
    Ok(removals)
}

/// Stops when a planned removal is refused.
pub(crate) fn check(removals: &[Removal]) -> Result<(), InstallError> {
    let refused: Vec<String> = removals.iter().filter_map(Removal::refusal).collect();
    if refused.is_empty() {
        return Ok(());
    }
    Err(InstallError::Message(format!(
        "apply changed nothing:\n  {}",
        refused.join("\n  ")
    )))
}

/// Deletes each component's `HelmChart` objects and waits until helm-controller has uninstalled
/// them.
pub(crate) fn run(kubectl: &Kubectl, removals: &[Removal]) -> Result<(), InstallError> {
    for removal in removals {
        out::step(&format!(
            "Removing {} ({})",
            removal.display_name, removal.id
        ));
        for args in &removal.prepare {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            kubectl.run(&args, None)?;
        }
        for chart in &removal.helm_charts {
            let deleted = kubectl.run(
                &[
                    "delete",
                    HELM_CHARTS,
                    chart,
                    "--namespace=kube-system",
                    "--ignore-not-found",
                    "--timeout=15m",
                ],
                None,
            );
            if let Err(error) = deleted {
                let job = format!("job/helm-delete-{chart}");
                let logs = kubectl
                    .run(
                        &["logs", "--namespace=kube-system", &job, "--tail=30"],
                        None,
                    )
                    .unwrap_or_default();
                return Err(InstallError::Message(format!(
                    "the {chart} chart was not uninstalled: {error}\n{logs}"
                )));
            }
        }
    }
    Ok(())
}

/// The installer's `HelmChart` objects; none when the cluster does not serve the resource yet.
fn installed(kubectl: &Kubectl) -> Result<Vec<Installed>, InstallError> {
    if kubectl
        .get("customresourcedefinition", HELM_CHARTS, None)?
        .is_none()
    {
        return Ok(Vec::new());
    }
    let selector = format!("--selector={}", remove::selector());
    let text = kubectl.run(
        &[
            "get",
            HELM_CHARTS,
            "--namespace=kube-system",
            &selector,
            "--output=json",
        ],
        None,
    )?;
    Ok(items(&text)?.iter().filter_map(remove::installed).collect())
}

fn used_by(
    kubectl: &Kubectl,
    removal: &Removal,
    together: &BTreeSet<String>,
    served: &BTreeSet<&str>,
) -> Result<Vec<String>, InstallError> {
    let elsewhere = |object: &Value| namespace(object).is_some_and(|ns| !together.contains(ns));
    let mut found = Vec::new();
    for check in &removal.in_use {
        match check {
            InUse::Objects { resource } if served.contains(resource.as_str()) => {
                // A cluster-scoped object goes only when its CRD goes.
                let deleted = removal.deleted_crds.contains(resource);
                for object in list(kubectl, resource)?
                    .iter()
                    .filter(|o| elsewhere(o) || (deleted && namespace(o).is_none()))
                {
                    let kind = object["kind"].as_str().unwrap_or(resource);
                    found.push(namespace(object).map_or_else(
                        || format!("{kind} {}", name(object)),
                        |ns| format!("{kind} {ns}/{}", name(object)),
                    ));
                }
            }
            InUse::Objects { .. } => {}
            InUse::Volumes { driver } => {
                for volume in list(kubectl, "persistentvolumes")?.iter().filter(|v| {
                    v.pointer("/spec/csi/driver").and_then(Value::as_str) == Some(driver)
                }) {
                    let claim = volume
                        .pointer("/spec/claimRef")
                        .map_or_else(String::new, |c| {
                            format!(
                                " (claim {}/{})",
                                c["namespace"].as_str().unwrap_or_default(),
                                c["name"].as_str().unwrap_or_default()
                            )
                        });
                    found.push(format!("PersistentVolume {}{claim}", name(volume)));
                }
            }
            InUse::Images { registry } => {
                let prefix = format!("{registry}/");
                for pod in list(kubectl, "pods")?.iter().filter(|p| elsewhere(p)) {
                    let images = ["containers", "initContainers"]
                        .iter()
                        .filter_map(|key| pod.pointer(&format!("/spec/{key}"))?.as_array())
                        .flatten()
                        .filter_map(|c| c["image"].as_str());
                    if images.into_iter().any(|image| image.starts_with(&prefix)) {
                        found.push(format!(
                            "Pod {}/{}",
                            namespace(pod).unwrap_or_default(),
                            name(pod)
                        ));
                    }
                }
            }
        }
    }
    if found.len() > MAX_NAMED {
        let more = found.len() - MAX_NAMED;
        found.truncate(MAX_NAMED);
        found.push(format!("{more} more"));
    }
    Ok(found)
}

fn list_cluster(kubectl: &Kubectl, resource: &str) -> Result<Vec<Value>, InstallError> {
    let text = kubectl.run(&["get", resource, "--output=json"], None)?;
    items(&text)
}

fn list(kubectl: &Kubectl, resource: &str) -> Result<Vec<Value>, InstallError> {
    let text = kubectl.run(
        &["get", resource, "--all-namespaces", "--output=json"],
        None,
    )?;
    items(&text)
}

fn items(text: &str) -> Result<Vec<Value>, InstallError> {
    let list: Value =
        serde_json::from_str(text).map_err(|e| InstallError::Message(e.to_string()))?;
    Ok(list
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn namespace(object: &Value) -> Option<&str> {
    object
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
}

fn name(object: &Value) -> &str {
    object
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .unwrap_or_default()
}
