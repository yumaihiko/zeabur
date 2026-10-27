//! Checks before anything is changed.

use std::fs;
use std::net::TcpListener;
use std::path::Path;

use serde::Serialize;
use zeabur_installer_plan::cluster::Distribution;
use zeabur_installer_plan::kubesolo;

use crate::{cmd, systemd};

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// Fine.
    Pass,
    /// Worth reading; does not stop `apply`.
    Warn,
    /// Stops `apply`.
    Fail,
}

/// Declares an enum whose variants each name a stable key, serialized as that key.
macro_rules! keyed {
    ($(#[$attr:meta])* pub enum $name:ident { $($variant:ident => $key:literal,)+ }) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $(#[doc = concat!("`", $key, "`.")] $variant,)+
        }

        impl $name {
            /// Every value.
            pub const ALL: &[Self] = &[$(Self::$variant),+];

            /// The stable key.
            #[must_use]
            pub const fn key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key,)+
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.key())
            }
        }
    };
}

keyed! {
    /// What a check looks at. An interface words it as `check.<key>`.
    pub enum Topic {
        Root => "root",
        Systemd => "systemd",
        Time => "time",
        Swap => "swap",
        CrashKernel => "crashKernel",
        OtherKubernetes => "otherKubernetes",
        Ports => "ports",
        DiskSpace => "diskSpace",
        DiskEncryption => "diskEncryption",
        NodePackages => "nodePackages",
        KernelModules => "kernelModules",
        Services => "services",
        Multipathd => "multipathd",
        Docker => "docker",
    }
}

impl Topic {
    /// The English name, for the command line.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Systemd => "systemd",
            Self::Time => "time",
            Self::Swap => "swap",
            Self::CrashKernel => "crash kernel",
            Self::OtherKubernetes => "other Kubernetes",
            Self::Ports => "ports",
            Self::DiskSpace => "disk space",
            Self::DiskEncryption => "disk encryption",
            Self::NodePackages => "node packages",
            Self::KernelModules => "kernel modules",
            Self::Services => "services",
            Self::Multipathd => "multipathd",
            Self::Docker => "Docker",
        }
    }
}

keyed! {
    /// How a check came out, as a sentence an interface words as `code.<key>` with the check's
    /// values.
    pub enum Code {
        RootOk => "root.ok",
        RootNo => "root.no",
        SystemdOk => "systemd.ok",
        SystemdNo => "systemd.no",
        TimeOk => "time.ok",
        TimeDrift => "time.drift",
        TimeUnknown => "time.unknown",
        SwapOff => "swap.off",
        SwapOn => "swap.on",
        SwapUnknown => "swap.unknown",
        CrashNone => "crash.none",
        CrashKept => "crash.kept",
        OthersNone => "others.none",
        OthersFound => "others.found",
        PortsFree => "ports.free",
        PortsBusy => "ports.busy",
        DiskUnknown => "disk.unknown",
        DiskLow => "disk.low",
        DiskTight => "disk.tight",
        DiskOk => "disk.ok",
        CryptYes => "crypt.yes",
        CryptRequiredPlain => "crypt.requiredPlain",
        CryptRequiredUnknown => "crypt.requiredUnknown",
        CryptNo => "crypt.no",
        CryptUnknown => "crypt.unknown",
        PackageOk => "prep.package.ok",
        PackageToInstall => "prep.package.toInstall",
        PackageMissing => "prep.package.missing",
        ModuleOk => "prep.module.ok",
        ModuleToLoad => "prep.module.toLoad",
        ModuleMissing => "prep.module.missing",
        ServiceOk => "prep.service.ok",
        ServiceToStart => "prep.service.toStart",
        ServiceStopped => "prep.service.stopped",
        MultipathToBlock => "prep.multipath.toBlock",
        MultipathOpen => "prep.multipath.open",
        DockerNone => "docker.none",
        DockerFound => "docker.found",
        ForwardDrop => "docker.forwardDrop",
    }
}

/// One check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// What was checked.
    pub name: Topic,
    /// The outcome.
    pub status: Status,
    /// What was found and what to do, in English.
    pub detail: String,
    /// The outcome as a key, for an interface that words it itself.
    pub code: Code,
    /// The values `code` names, in order: names, paths, numbers; never words.
    pub values: Vec<String>,
}

/// Whether a disk is encrypted, as far as the block devices tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// A dm-crypt device is under it.
    Encrypted,
    /// No dm-crypt device is under it.
    Plain,
    /// The devices could not be read.
    Unknown,
}

/// What the checks need to know.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The cluster from an earlier `apply` is running, so its ports are expected to be in use.
    pub cluster_running: bool,
    /// Fail on a disk that is not encrypted.
    pub require_encrypted_disk: bool,
    /// What this host runs.
    pub node: Node,
}

/// What a host runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    /// A server, which also listens on the API and join ports.
    Server(Distribution),
    /// A worker.
    Agent(Distribution),
    /// `KubeSolo`, for a personal install.
    KubeSolo,
}

impl Node {
    /// The distribution id, as the `OTHERS` list names it.
    const fn kind(self) -> &'static str {
        match self {
            Self::Server(d) | Self::Agent(d) => d.id(),
            Self::KubeSolo => "kubesolo",
        }
    }

    const fn data_dir(self) -> &'static str {
        match self {
            Self::Server(_) | Self::Agent(_) => "/var/lib/rancher",
            Self::KubeSolo => kubesolo::DATA_DIR,
        }
    }
}

/// Files left by Kubernetes distributions, and the id of the distribution that owns each, which
/// an install of the same distribution does not report.
const OTHERS: [(&str, &str, Option<&str>); 9] = [
    ("/usr/local/bin/k3s", "K3s", Some("k3s")),
    ("/etc/rancher/k3s", "K3s", Some("k3s")),
    ("/etc/kubernetes/admin.conf", "kubeadm", None),
    ("/var/lib/k0s", "k0s", None),
    ("/snap/bin/microk8s", "MicroK8s", None),
    ("/usr/local/bin/rke2", "RKE2", Some("rke2")),
    ("/etc/rancher/rke2", "RKE2", Some("rke2")),
    (kubesolo::BINARY, "KubeSolo", Some("kubesolo")),
    (kubesolo::DATA_DIR, "KubeSolo", Some("kubesolo")),
];
const DISK_NEEDED_GIB: u64 = 10;
const DISK_ROOMY_GIB: u64 = 30;

/// Runs every check.
#[must_use]
pub fn run(options: Options) -> Vec<Check> {
    let mut checks = vec![
        root(),
        systemd(),
        time_sync(),
        swap(),
        crash_kernel(),
        other_distributions(options.node),
    ];
    if options.node == Node::KubeSolo {
        checks.push(docker());
    }
    if !options.cluster_running {
        checks.push(ports(options.node));
    }
    checks.push(disk_space(options.node.data_dir()));
    checks.push(encryption(
        options.node.data_dir(),
        options.require_encrypted_disk,
    ));
    checks
}

fn check(name: Topic, status: Status, detail: impl Into<String>, code: Code) -> Check {
    Check::new(name, status, detail, code)
}

impl Check {
    pub(crate) fn new(name: Topic, status: Status, detail: impl Into<String>, code: Code) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
            code,
            values: Vec::new(),
        }
    }

    pub(crate) fn values(mut self, values: &[&str]) -> Self {
        self.values = values.iter().map(|&v| v.to_owned()).collect();
        self
    }
}

/// Whether this process runs as root.
#[must_use]
pub fn is_root() -> bool {
    effective_uid(&fs::read_to_string("/proc/self/status").unwrap_or_default()) == Some(0)
}

fn root() -> Check {
    if is_root() {
        check(Topic::Root, Status::Pass, "running as root", Code::RootOk)
    } else {
        check(
            Topic::Root,
            Status::Fail,
            "run apply as root (sudo)",
            Code::RootNo,
        )
    }
}

fn effective_uid(status: &str) -> Option<u32> {
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

fn systemd() -> Check {
    if Path::new("/run/systemd/system").is_dir() {
        check(
            Topic::Systemd,
            Status::Pass,
            "systemd is the init system",
            Code::SystemdOk,
        )
    } else {
        check(
            Topic::Systemd,
            Status::Fail,
            "Kubernetes runs as a systemd service; this host does not run systemd",
            Code::SystemdNo,
        )
    }
}

fn time_sync() -> Check {
    match cmd::run(
        "timedatectl",
        &["show", "--property=NTPSynchronized", "--value"],
        None,
    ) {
        Ok(out) if out.trim() == "yes" => check(
            Topic::Time,
            Status::Pass,
            "the clock is synchronised",
            Code::TimeOk,
        ),
        Ok(_) => check(
            Topic::Time,
            Status::Warn,
            "the clock is not synchronised; certificates and tokens fail when it drifts (timedatectl set-ntp true)",
            Code::TimeDrift,
        ),
        Err(_) => check(
            Topic::Time,
            Status::Warn,
            "could not ask timedatectl whether the clock is synchronised",
            Code::TimeUnknown,
        ),
    }
}

fn swap() -> Check {
    match fs::read_to_string("/proc/swaps") {
        Ok(swaps) if swaps.lines().skip(1).any(|l| !l.trim().is_empty()) => check(
            Topic::Swap,
            Status::Warn,
            "swap is on; the kubelet expects it off (swapoff -a, and remove it from /etc/fstab)",
            Code::SwapOn,
        ),
        Ok(_) => check(Topic::Swap, Status::Pass, "swap is off", Code::SwapOff),
        Err(_) => check(
            Topic::Swap,
            Status::Warn,
            "could not read /proc/swaps",
            Code::SwapUnknown,
        ),
    }
}

fn crash_kernel() -> Check {
    crash_reserve(
        fs::read_to_string("/sys/kernel/kexec_crash_size")
            .ok()
            .as_deref(),
    )
}

fn crash_reserve(raw: Option<&str>) -> Check {
    let mib = raw
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map_or(0, |bytes| bytes >> 20);
    if mib == 0 {
        return check(
            Topic::CrashKernel,
            Status::Pass,
            "no memory kept for a crash kernel",
            Code::CrashNone,
        );
    }
    check(
        Topic::CrashKernel,
        Status::Warn,
        format!("the kernel keeps {mib} MiB for a crash kernel (kdump); to free it: sudo apt purge kdump-tools && sudo update-grub, then reboot"),
        Code::CrashKept,
    )
    .values(&[&mib.to_string()])
}

fn other_distributions(node: Node) -> Check {
    let found: Vec<(&str, &str)> = OTHERS
        .iter()
        .filter(|(_, _, own)| own.is_none_or(|own| own != node.kind()))
        .filter(|(path, ..)| Path::new(path).exists())
        .map(|&(path, name, _)| (path, name))
        .collect();
    if found.is_empty() {
        check(
            Topic::OtherKubernetes,
            Status::Pass,
            "none found",
            Code::OthersNone,
        )
    } else {
        let named: Vec<String> = found
            .iter()
            .map(|(path, name)| format!("{name} ({path})"))
            .collect();
        let paths: Vec<&str> = found.iter().map(|(path, _)| *path).collect();
        check(
            Topic::OtherKubernetes,
            Status::Warn,
            format!(
                "found files of {}; if it still runs, uninstall it first",
                named.join(", ")
            ),
            Code::OthersFound,
        )
        .values(&paths)
    }
}

/// Where Docker's binary and socket are. Docker sets the iptables FORWARD policy to DROP, so
/// Pods of `KubeSolo` cannot reach the internet and ACME fails.
const DOCKER_PATHS: [&str; 5] = [
    "/usr/bin/docker",
    "/usr/local/bin/docker",
    "/snap/bin/docker",
    "/run/docker.sock",
    "/var/run/docker.sock",
];
const DOCKER_UNITS: [&str; 2] = ["docker.service", "docker.socket"];

fn docker() -> Check {
    let mut found: Vec<String> = DOCKER_PATHS
        .iter()
        .filter(|path| Path::new(path).exists())
        .map(|&path| path.to_owned())
        .collect();
    found.extend(
        DOCKER_UNITS
            .iter()
            .filter(|unit| systemd::is_active(unit))
            .map(|&unit| unit.to_owned()),
    );
    let iptables = cmd::run("iptables", &["-S", "FORWARD"], None).unwrap_or_default();
    docker_check(&found, &iptables)
}

fn docker_check(found: &[String], iptables: &str) -> Check {
    if !found.is_empty() {
        let names: Vec<&str> = found.iter().map(String::as_str).collect();
        check(
            Topic::Docker,
            Status::Fail,
            format!(
                "Docker is on this host ({}). It sets the iptables FORWARD policy to DROP, so Pods cannot reach the internet and certificates fail. Stop and remove Docker first",
                names.join(", ")
            ),
            Code::DockerFound,
        )
        .values(&names)
    } else if iptables.lines().any(|l| l.trim() == "-P FORWARD DROP") {
        check(
            Topic::Docker,
            Status::Fail,
            "the iptables FORWARD policy is DROP, so Pods cannot reach the internet and certificates fail; find what sets it (Docker, ufw) and change it",
            Code::ForwardDrop,
        )
    } else {
        check(
            Topic::Docker,
            Status::Pass,
            "no Docker, and the iptables FORWARD policy is not DROP",
            Code::DockerNone,
        )
    }
}

/// The TCP ports a node needs free.
#[must_use]
pub fn needed_ports(node: Node) -> Vec<(u16, &'static str)> {
    match node {
        Node::Server(d) => d.node_ports(true),
        Node::Agent(d) => d.node_ports(false),
        Node::KubeSolo => kubesolo::ports(),
    }
}

fn ports(node: Node) -> Check {
    let ports = needed_ports(node);
    let busy: Vec<&(u16, &str)> = ports
        .iter()
        .filter(|(port, _)| TcpListener::bind(("0.0.0.0", *port)).is_err())
        .collect();
    if busy.is_empty() {
        let numbers: Vec<String> = ports.iter().map(|(port, _)| port.to_string()).collect();
        check(
            Topic::Ports,
            Status::Pass,
            format!("{} are free", and_list(&numbers)),
            Code::PortsFree,
        )
        .values(&numbers.iter().map(String::as_str).collect::<Vec<_>>())
    } else {
        let named: Vec<String> = busy
            .iter()
            .map(|(port, what)| format!("{port} ({what})"))
            .collect();
        let numbers: Vec<String> = busy.iter().map(|(port, _)| port.to_string()).collect();
        check(
            Topic::Ports,
            Status::Fail,
            format!("in use: {}; `ss -ltnp` shows by what", named.join(", ")),
            Code::PortsBusy,
        )
        .values(&numbers.iter().map(String::as_str).collect::<Vec<_>>())
    }
}

/// `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn existing_or_var_lib(dir: &str) -> &str {
    if Path::new(dir).exists() {
        dir
    } else {
        "/var/lib"
    }
}

fn disk_space(data_dir: &str) -> Check {
    let target = existing_or_var_lib(data_dir);
    let Some(kib) = cmd::run("df", &["-Pk", target], None)
        .ok()
        .and_then(|out| available_kib(&out))
    else {
        return check(
            Topic::DiskSpace,
            Status::Warn,
            format!("could not read the free space of {target}"),
            Code::DiskUnknown,
        )
        .values(&[target]);
    };
    let gib = gib(kib);
    if kib < DISK_NEEDED_GIB << 20 {
        check(
            Topic::DiskSpace,
            Status::Fail,
            format!("{gib} GiB free under {target}; the cluster's images and data need at least {DISK_NEEDED_GIB} GiB"),
            Code::DiskLow,
        )
        .values(&[&gib, target, &DISK_NEEDED_GIB.to_string()])
    } else if kib < DISK_ROOMY_GIB << 20 {
        check(
            Topic::DiskSpace,
            Status::Warn,
            format!(
                "{gib} GiB free under {target}; {DISK_ROOMY_GIB} GiB or more leaves room for logs, metrics and apps"
            ),
            Code::DiskTight,
        )
        .values(&[&gib, target, &DISK_ROOMY_GIB.to_string()])
    } else {
        check(
            Topic::DiskSpace,
            Status::Pass,
            format!("{gib} GiB free under {target}"),
            Code::DiskOk,
        )
        .values(&[&gib, target])
    }
}

/// KiB as GiB with one decimal, rounded down.
fn gib(kib: u64) -> String {
    let tenths = kib * 10 / (1 << 20);
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn available_kib(df: &str) -> Option<u64> {
    df.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()
}

fn encryption(data_dir: &str, required: bool) -> Check {
    let target = existing_or_var_lib(data_dir);
    let found = disk_encryption(target);
    let (status, detail, code) = match (found, required) {
        (Encryption::Encrypted, _) => (
            Status::Pass,
            format!("{target} is on a dm-crypt device"),
            Code::CryptYes,
        ),
        (Encryption::Plain, true) => (
            Status::Fail,
            format!("{target} is not on an encrypted device, and host.requireEncryptedDisk is set"),
            Code::CryptRequiredPlain,
        ),
        (Encryption::Unknown, true) => (
            Status::Fail,
            format!(
                "could not tell whether {target} is encrypted, and host.requireEncryptedDisk is set"
            ),
            Code::CryptRequiredUnknown,
        ),
        (Encryption::Plain, false) => (
            Status::Warn,
            format!(
                "{target} is not on an encrypted disk, so volumes and databases are stored \
                 unencrypted; most VPS plans can't encrypt it afterwards, so for sensitive data \
                 choose a plan whose provider encrypts disks"
            ),
            Code::CryptNo,
        ),
        (Encryption::Unknown, false) => (
            Status::Warn,
            format!("could not tell whether {target} is encrypted"),
            Code::CryptUnknown,
        ),
    };
    check(Topic::DiskEncryption, status, detail, code).values(&[target])
}

/// Whether the device holding `path` has a dm-crypt device beneath it.
#[must_use]
pub fn disk_encryption(path: &str) -> Encryption {
    let Ok(source) = cmd::run(
        "findmnt",
        &["--noheadings", "--output", "SOURCE", "--target", path],
        None,
    ) else {
        return Encryption::Unknown;
    };
    let source = source.trim();
    // Btrfs subvolumes read as `/dev/sda2[/@]`.
    let device = source.split('[').next().unwrap_or(source);
    match cmd::run(
        "lsblk",
        &["--noheadings", "--inverse", "--output", "TYPE", device],
        None,
    ) {
        Ok(types) if types.split_whitespace().any(|t| t == "crypt") => Encryption::Encrypted,
        Ok(_) => Encryption::Plain,
        Err(_) => Encryption::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_or_a_dropping_forward_policy_stops_the_install() {
        let found = vec!["/usr/bin/docker".to_owned()];
        let policy = "-P FORWARD ACCEPT\n";
        let drop = "-P FORWARD DROP\n-A FORWARD -j DOCKER-USER\n";
        let case = |found: &[String], iptables: &str| {
            let c = docker_check(found, iptables);
            (c.status, c.code)
        };
        assert_eq!(case(&[], policy), (Status::Pass, Code::DockerNone));
        assert_eq!(case(&[], ""), (Status::Pass, Code::DockerNone));
        assert_eq!(case(&found, policy), (Status::Fail, Code::DockerFound));
        assert_eq!(case(&[], drop), (Status::Fail, Code::ForwardDrop));
        assert_eq!(case(&found, drop), (Status::Fail, Code::DockerFound));
        assert_eq!(docker_check(&found, policy).values, ["/usr/bin/docker"]);
    }

    #[test]
    fn parsing() {
        assert_eq!(effective_uid("Name:\tx\nUid:\t1000\t0\t0\t0\n"), Some(0));
        assert_eq!(effective_uid("Uid:\t0\t1000\t1000\t1000\n"), Some(1000));
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda2 61255492 9032 49094708 16% /\n";
        assert_eq!(available_kib(df), Some(49_094_708));
        assert_eq!(gib(3_250_586), "3.1");
        assert_eq!(gib(10 << 20), "10.0");
        let crash = |raw| {
            let c = crash_reserve(raw);
            (c.status, c.code)
        };
        assert_eq!(crash(None), (Status::Pass, Code::CrashNone));
        assert_eq!(crash(Some("0\n")), (Status::Pass, Code::CrashNone));
        assert_eq!(crash(Some("536870912\n")), (Status::Warn, Code::CrashKept));
        assert_eq!(crash_reserve(Some("536870912\n")).values, ["512"]);
        let ports = |node| -> Vec<String> {
            needed_ports(node)
                .iter()
                .map(|(p, _)| p.to_string())
                .collect()
        };
        assert_eq!(
            and_list(&ports(Node::Agent(Distribution::Rke2))),
            "80, 443 and 10250"
        );
        assert_eq!(
            and_list(&ports(Node::Server(Distribution::Rke2))),
            "80, 443, 6443, 9345 and 10250"
        );
        assert_eq!(
            and_list(&ports(Node::Server(Distribution::K3s))),
            "80, 443, 6443 and 10250"
        );
        assert_eq!(
            and_list(&ports(Node::KubeSolo)),
            "80, 443, 6443, 10250 and 10443"
        );
    }
}
