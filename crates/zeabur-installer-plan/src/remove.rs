//! Removing components that left the configuration.
//!
//! The installer removes only what it installed: the `HelmChart` objects in `kube-system` with
//! its `app.kubernetes.io/managed-by` label and the [`COMPONENT_ANNOTATION`]. Deleting one makes
//! helm-controller run `helm uninstall`. What Helm leaves stays: the CRDs and volumes the catalog
//! lists in `leftovers`, the objects of manifest releases, and the component's namespaces with
//! their Secrets.
//!
//! `apply` refuses to remove a component that something still uses ([`InUse`]), and one that
//! keeps data without volumes that stay, unless `allowDataLoss` names it.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::catalog::{Component, Index};
use crate::plan::{COMPONENT_ANNOTATION, ComponentPlan, MANAGER, Warning, ZOT, warning};
use crate::rke2::ZOT_HOST;
use crate::storage::LONGHORN;

/// The leftover that means the chart's volumes stay.
const VOLUMES: &str = "persistentvolumeclaims";
/// Components whose custom resources are their own output, not something that uses them.
const OUTPUT_ONLY: &[&str] = &["vulnerability-scanning/trivy-operator"];
/// Longhorn's CSI driver.
const LONGHORN_DRIVER: &str = "driver.longhorn.io";
/// The Longhorn setting its uninstall job reads; the job fails while it is `false`.
const LONGHORN_CONFIRMATION: &str = "deleting-confirmation-flag";

/// A `HelmChart` the installer made, as found in the cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The component in its [`COMPONENT_ANNOTATION`].
    pub component: String,
    /// The `HelmChart`'s name.
    pub chart: String,
}

/// One component to remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Removal {
    /// `<capability>/<upstream>`.
    pub id: String,
    /// Name for people.
    pub display_name: String,
    /// The `HelmChart` objects in `kube-system` that `apply` deletes.
    pub helm_charts: Vec<String>,
    /// Namespaces that stay, with what is left in them.
    pub namespaces: Vec<String>,
    /// What the catalog says `helm uninstall` leaves.
    pub leftovers: Vec<String>,
    /// Manifest releases, whose objects stay.
    pub manifests: Vec<String>,
    /// What `apply` looks for before it removes the component.
    pub in_use: Vec<InUse>,
    /// What still uses it, found by the installer; any entry stops `apply`.
    pub used_by: Vec<String>,
    /// Removing it deletes data it keeps.
    pub loses_data: bool,
    /// `allowDataLoss` names it.
    pub confirmed: bool,
    /// `kubectl` arguments run before the `HelmChart` objects are deleted.
    pub prepare: Vec<Vec<String>>,
}

impl Removal {
    /// Why `apply` does not remove it, if it does not.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        if !self.used_by.is_empty() {
            Some(format!(
                "{} is still in use: {}",
                self.id,
                self.used_by.join(", ")
            ))
        } else if self.loses_data && !self.confirmed {
            Some(format!(
                "removing {} deletes the data it keeps; to remove it anyway, add it to \
                 allowDataLoss in the configuration",
                self.id
            ))
        } else {
            None
        }
    }
}

/// What uses a component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum InUse {
    /// Namespaced objects of a CRD (`<plural>.<group>`), outside the namespaces of the components
    /// removed together.
    Objects {
        /// The resource.
        resource: String,
    },
    /// `PersistentVolume` objects of a CSI driver.
    Volumes {
        /// The driver.
        driver: String,
    },
    /// Pods that run images from a registry, outside the namespaces of the components removed
    /// together.
    Images {
        /// `host:port`.
        registry: String,
    },
}

/// The label selector that finds the installer's `HelmChart` objects.
#[must_use]
pub fn selector() -> String {
    format!("app.kubernetes.io/managed-by={MANAGER}")
}

/// The [`Installed`] of a `HelmChart` object; `None` for one without the annotation, such as
/// Zeabur's own.
#[must_use]
pub fn installed(object: &serde_json::Value) -> Option<Installed> {
    let metadata = object.get("metadata")?;
    Some(Installed {
        component: metadata
            .pointer(&format!(
                "/annotations/{}",
                COMPONENT_ANNOTATION.replace('/', "~1")
            ))?
            .as_str()?
            .to_owned(),
        chart: metadata.get("name")?.as_str()?.to_owned(),
    })
}

/// The components installed but no longer planned, in the order to remove them: a component
/// before what it needs.
///
/// A component the catalog no longer has is not removed; a warning says how to remove it by hand.
#[must_use]
pub fn removals(
    index: &Index,
    planned: &[ComponentPlan],
    installed: &[Installed],
    profile: &str,
    allow_data_loss: &[String],
) -> (Vec<Removal>, Vec<Warning>) {
    let planned: BTreeSet<&str> = planned.iter().map(|c| c.id.as_str()).collect();
    let gone: BTreeSet<&str> = installed
        .iter()
        .map(|i| i.component.as_str())
        .filter(|id| !planned.contains(id))
        .collect();
    let mut warnings = Vec::new();
    let mut order: Vec<&Component> = Vec::new();
    for id in &gone {
        let Some(component) = index.component(id) else {
            let charts: Vec<&str> = installed
                .iter()
                .filter(|i| i.component == *id)
                .map(|i| i.chart.as_str())
                .collect();
            warnings.push(warning(
                "remove.unknown",
                &[id, &charts.join(" ")],
                format!(
                    "{id} is installed but not in this catalog, so apply leaves it. To remove \
                     it: kubectl -n kube-system delete helmchart {}",
                    charts.join(" ")
                ),
            ));
            continue;
        };
        let chain = component
            .install_order
            .get(profile)
            .cloned()
            .unwrap_or_else(|| vec![component.id.clone()]);
        for dep in chain {
            if gone.contains(dep.as_str()) && !order.iter().any(|c| c.id == dep) {
                order.extend(index.component(&dep));
            }
        }
    }
    order.reverse();
    let removals = order
        .into_iter()
        .map(|component| removal(component, installed, allow_data_loss))
        .collect();
    (removals, warnings)
}

fn removal(component: &Component, installed: &[Installed], allow_data_loss: &[String]) -> Removal {
    let id = component.id.as_str();
    let mut namespaces: Vec<String> = Vec::new();
    for release in &component.releases {
        if release.namespace != "kube-system" && !namespaces.contains(&release.namespace) {
            namespaces.push(release.namespace.clone());
        }
    }
    let in_use = if id == LONGHORN {
        vec![InUse::Volumes {
            driver: LONGHORN_DRIVER.to_owned(),
        }]
    } else if id == ZOT {
        vec![InUse::Images {
            registry: ZOT_HOST.to_owned(),
        }]
    } else if OUTPUT_ONLY.contains(&id) {
        Vec::new()
    } else {
        component
            .detection
            .crds
            .iter()
            .map(|crd| InUse::Objects {
                resource: crd.clone(),
            })
            .collect()
    };
    let loses_data = component.persists_data && !component.leftovers.iter().any(|l| l == VOLUMES);
    let confirmed = allow_data_loss.iter().any(|a| a == id);
    let prepare = if id == LONGHORN && confirmed {
        let namespace = namespaces.first().map_or("longhorn-system", String::as_str);
        vec![
            [
                "patch",
                "--namespace",
                namespace,
                "settings.longhorn.io",
                LONGHORN_CONFIRMATION,
                "--type=merge",
                "--patch",
                r#"{"value":"true"}"#,
            ]
            .map(str::to_owned)
            .to_vec(),
        ]
    } else {
        Vec::new()
    };
    Removal {
        id: id.to_owned(),
        display_name: component.metadata.name.clone(),
        helm_charts: installed
            .iter()
            .filter(|i| i.component == id)
            .map(|i| i.chart.clone())
            .collect(),
        namespaces,
        leftovers: component.leftovers.clone(),
        manifests: component
            .releases
            .iter()
            .filter(|r| r.chart.is_none())
            .map(|r| r.name.clone())
            .collect(),
        in_use,
        used_by: Vec::new(),
        loses_data,
        confirmed,
        prepare,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::Config;
    use crate::plan::{Facts, plan};

    const INDEX: &str = include_str!("../fixtures/index.json");

    fn index() -> Index {
        Index::parse(INDEX.as_bytes()).unwrap()
    }

    fn planned(extra: &str) -> Vec<ComponentPlan> {
        let config = Config::parse(&format!(
            "access: {{mode: domain, identity: login.example.com, zeabur: zeabur.example.com}}\n\
             sources:\n  identityImage: ghcr.io/zeabur-oss/identity@sha256:aa\n  identityctl: /tmp/ctl\n  \
             zeaburImage: zeabur:dev\n  zeaburChart: /tmp/chart\n{extra}"
        ))
        .unwrap();
        let facts = Facts {
            arch: "arm64".to_owned(),
            hostname: "vps-1".to_owned(),
            memory_mib: 8192,
            node_ips: Vec::new(),
        };
        plan(&index(), &config, &facts).unwrap().components
    }

    fn installed(planned: &[ComponentPlan]) -> Vec<Installed> {
        planned
            .iter()
            .flat_map(|c| {
                c.releases.iter().filter_map(|r| match r {
                    crate::plan::ReleasePlan::HelmChart { name, .. } => Some(Installed {
                        component: c.id.clone(),
                        chart: name.clone(),
                    }),
                    crate::plan::ReleasePlan::Manifests { .. }
                    | crate::plan::ReleasePlan::Urls { .. } => None,
                })
            })
            .collect()
    }

    #[test]
    fn what_apply_installed_it_finds_again_by_the_label_and_annotation() {
        for component in planned("enable: [replicated-storage/longhorn]\n") {
            for release in &component.releases {
                if let crate::plan::ReleasePlan::HelmChart { name, object, .. } = release {
                    assert_eq!(
                        object["metadata"]["labels"]["app.kubernetes.io/managed-by"],
                        selector().split_once('=').unwrap().1
                    );
                    assert_eq!(
                        super::installed(object),
                        Some(Installed {
                            component: component.id.clone(),
                            chart: name.clone(),
                        })
                    );
                }
            }
        }
        let zeabur = json!({"metadata": {"name": "zeabur", "labels": {"app.kubernetes.io/managed-by": MANAGER}}});
        assert_eq!(super::installed(&zeabur), None);
    }

    #[test]
    fn nothing_is_removed_while_the_configuration_keeps_it() {
        let now = planned("enable: [replicated-storage/longhorn]\n");
        let (removals, warnings) = removals(&index(), &now, &installed(&now), "single-node", &[]);
        assert_eq!(removals, []);
        assert_eq!(warnings, []);
    }

    #[test]
    fn a_component_left_out_is_removed_before_what_it_needs() {
        let before = planned("preset: minimal\nenable: [metrics/victoria-metrics]\n");
        let after = planned("preset: minimal\n");
        let (removals, _) = removals(&index(), &after, &installed(&before), "single-node", &[]);
        let ids: Vec<&str> = removals.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "metrics/victoria-metrics",
                "observability-operator/victoria-metrics-operator"
            ]
        );
        let metrics = &removals[0];
        assert_ne!(metrics.helm_charts, Vec::<String>::new());
        assert!(metrics.leftovers.iter().any(|l| l == VOLUMES));
        assert!(!metrics.loses_data);
        assert_eq!(metrics.refusal(), None);
    }

    #[test]
    fn removing_longhorn_needs_allow_data_loss_and_looks_for_its_volumes() {
        let before = planned("enable: [replicated-storage/longhorn]\n");
        let after = planned("");
        let refused = removals(&index(), &after, &installed(&before), "single-node", &[]).0;
        let longhorn = refused.iter().find(|r| r.id == LONGHORN).unwrap();
        assert!(longhorn.loses_data);
        assert!(longhorn.refusal().unwrap().contains("allowDataLoss"));
        assert_eq!(longhorn.prepare, Vec::<Vec<String>>::new());
        assert_eq!(
            longhorn.in_use,
            [InUse::Volumes {
                driver: LONGHORN_DRIVER.to_owned()
            }]
        );

        let allowed = removals(
            &index(),
            &after,
            &installed(&before),
            "single-node",
            &[LONGHORN.to_owned()],
        )
        .0;
        let mut longhorn = allowed.into_iter().find(|r| r.id == LONGHORN).unwrap();
        assert_eq!(longhorn.refusal(), None);
        assert!(
            longhorn.prepare[0]
                .iter()
                .any(|a| a == LONGHORN_CONFIRMATION)
        );
        longhorn.used_by = vec!["PersistentVolume pvc-1 (claim shop/data)".to_owned()];
        assert!(longhorn.refusal().unwrap().contains("pvc-1"));
    }

    #[test]
    fn an_operator_is_in_use_while_its_objects_exist() {
        let before = planned("enable: [event-autoscaling/keda]\n");
        let after = planned("");
        let removals = removals(&index(), &after, &installed(&before), "single-node", &[]).0;
        let keda = removals
            .iter()
            .find(|r| r.id == "event-autoscaling/keda")
            .unwrap();
        assert_eq!(
            keda.in_use,
            [InUse::Objects {
                resource: "scaledobjects.keda.sh".to_owned()
            }]
        );
        assert!(!keda.loses_data);
    }

    #[test]
    fn a_component_not_in_the_catalog_stays_with_a_warning() {
        let now = planned("");
        let mut charts = installed(&now);
        charts.push(Installed {
            component: "old/thing".to_owned(),
            chart: "thing".to_owned(),
        });
        let (removals, warnings) = removals(&index(), &now, &charts, "single-node", &[]);
        assert_eq!(removals, []);
        assert!(warnings[0].text.contains("delete helmchart thing"));
    }
}
