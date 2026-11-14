//! `hydo k8s` (Milestone 243): a project's Kubernetes manifests, a Helm chart or a Kustomize
//! folder become a starting hydo.yaml, as `hydo compose` makes one from a Compose file.
//! Kubernetes itself does not run in hydo: each pod's containers become hydo services. The design
//! and the mapping table are in docs/design/k8s-import.md.
//!
//! The conversion makes a document shaped as a Compose file, with `x-hydo-*` keys for what
//! Compose cannot say (see [`crate::Origin`]), and hands it to the Compose importer. So the images
//! are resolved and pinned by digest, the lock file and fetch-images.sh are written, and the
//! volumes, mounts, restart policies and one-shot jobs are written exactly as `hydo compose`
//! writes them.
//!
//! Files it writes next to the output, in `k8s-import/`:
//! - `configmap/<name>/...`: the ConfigMaps that a pod mounts, one file per key;
//! - `secrets/`: the Secrets that a pod mounts, and `<service>.env` with the environment values
//!   that come from Secrets. The folder holds a `.gitignore` of `*`, so no value of a Secret
//!   reaches version control; hydo.yaml names only the paths (`env_file:`, `root.files`);
//! - `manifests.yaml`: what `helm template` or `kubectl kustomize` printed, with the values of
//!   every Secret left out (only when the import rendered a chart or a Kustomize folder).

use crate::pyval::{as_list, Y};
use crate::{int_of, registry, safe_name, svc_name, yaml, ComposeError, Importer, Opts, Origin};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const TOOL: &str = "hydo k8s";

pub const USAGE: &str =
    "hydo k8s: turn Kubernetes manifests, a Helm chart or a Kustomize folder into a starting
hydo.yaml (MANIFEST.md \"From Kubernetes manifests\"; docs/design/k8s-import.md).

    hydo k8s <file|dir ...> [--out hydo.yaml] [--name NAME] [--namespace NS]
             [--helm CHART] [--values FILE ...] [--set KEY=VALUE ...] [--release NAME]
             [--kustomize DIR] [--images-dir DIR] [--lock FILE] [--no-resolve]
             [--network bridge|loopback] [--no-addr]

Inputs: manifest files (.yaml, .yml, .json; several documents per file; kind: List too), or a
folder: a folder with Chart.yaml is rendered with `helm template`, one with kustomization.yaml
with `kubectl kustomize`, any other folder gives every manifest file under it. `--helm CHART`
names a chart helm can find (a folder, repo/name or oci://...), `--values` and `--set` go to
helm, `--release` is the release name (default: hydo). `--kustomize DIR` renders a Kustomize
folder. No cluster is ever contacted; helm and kubectl only render.

What it does (the table in docs/design/k8s-import.md lists every field):
  * Deployment, ReplicaSet, DaemonSet, Pod: one pod; StatefulSet: one service per replica
    (<name>-0, <name>-1, ...) and a group of them (groups:, so K=<name>.majority works);
    Job: a one-shot job run once in provision:; CronJob: a one-shot job, its timetable a note;
    Helm hooks: pre-install jobs run before the pods, post-install jobs in provision:, tests are
    left out.
  * containers -> services; init containers -> one-shot jobs run in the setup: of the pod's
    services; command/args -> entrypoint/cmd ($(VAR) expanded); env, envFrom from ConfigMaps and
    Secrets (the Secret values go to a git-ignored env_file:, never into hydo.yaml).
  * Services -> names in /etc/hosts (name, name.namespace, ...svc.cluster.local, and the pod
    names of a headless Service of a StatefulSet); the targeted container port -> port:.
  * readinessProbe (else startupProbe): httpGet -> a wget, tcpSocket -> ready: tcp:<port>,
    exec -> a command inside the image; no probe -> ready: tcp:<first port>.
  * volumes: persistentVolumeClaim, volumeClaimTemplates, emptyDir -> directories on the data
    disk (the fault target), empty at first as in Kubernetes; configMap, secret, projected ->
    files copied into the image root; securityContext.fsGroup -> the setup: makes them the
    group's; runAsUser/runAsGroup -> user:; resources.limits.memory -> the guest RAM guess;
    restartPolicy -> restart:; images pinned by digest as hydo compose does.
It warns, one line each, about what does not map: Ingress, NetworkPolicy, custom resources and
their operators, service-mesh sidecars, scheduling (affinity, nodeSelector, tolerations), a
Service port that differs from its target port, host paths, and the like.";

const WORKLOADS: &[&str] = &[
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "ReplicaSet",
    "ReplicationController",
    "Job",
    "CronJob",
    "Pod",
];

/// Kinds that hydo needs nothing of: one note each.
const QUIET: &[(&str, &str)] = &[
    (
        "ServiceAccount",
        "there is no API server in hydo, so service accounts and their tokens are not needed",
    ),
    ("Role", "there is no API server in hydo; RBAC is not needed"),
    (
        "RoleBinding",
        "there is no API server in hydo; RBAC is not needed",
    ),
    (
        "ClusterRole",
        "there is no API server in hydo; RBAC is not needed",
    ),
    (
        "ClusterRoleBinding",
        "there is no API server in hydo; RBAC is not needed",
    ),
    (
        "PodDisruptionBudget",
        "hydo's faults are not evictions, so a disruption budget does not limit them",
    ),
    (
        "HorizontalPodAutoscaler",
        "the number of pods is fixed in hydo",
    ),
    ("VerticalPodAutoscaler", "resources are fixed in hydo"),
    (
        "Namespace",
        "namespaces only qualify names in hydo (name.namespace resolves)",
    ),
    (
        "PersistentVolume",
        "a claim is a directory on the data disk in hydo",
    ),
    (
        "StorageClass",
        "a claim is a directory on the data disk in hydo",
    ),
    ("LimitRange", "not modelled"),
    ("ResourceQuota", "not modelled"),
    (
        "PriorityClass",
        "there is one machine; priorities do not apply",
    ),
    ("PodSecurityPolicy", "not modelled"),
];

/// Kinds that do not map: one warning each, with what to do.
const REFUSED: &[(&str, &str)] = &[
    ("Ingress", "there is no ingress controller in hydo: the workload reaches the Services by their names"),
    ("IngressClass", "there is no ingress controller in hydo"),
    ("Gateway", "there is no gateway controller in hydo: the workload reaches the Services by their names"),
    ("HTTPRoute", "there is no gateway controller in hydo"),
    ("GRPCRoute", "there is no gateway controller in hydo"),
    ("NetworkPolicy", "isolation between pods is not modelled: every service reaches every other; hydo's network faults (N=, O=) cut links on purpose instead"),
    ("CustomResourceDefinition", "custom resources need their operator, which does not run in hydo; import what the operator would create (its StatefulSets and Services) instead"),
    ("MutatingWebhookConfiguration", "admission webhooks do not run, so nothing they would inject (a sidecar, a setting) is added"),
    ("ValidatingWebhookConfiguration", "admission webhooks do not run"),
    ("APIService", "there is no API server in hydo"),
    ("Endpoints", "hand-written endpoints point at addresses outside the guest; a Service without a selector does not resolve (make the outside system a fake, fakes:)"),
    ("EndpointSlice", "hand-written endpoints point at addresses outside the guest"),
];

/// Containers that a service mesh injects: left out.
const MESH: &[&str] = &[
    "istio-proxy",
    "istio-init",
    "istio-validation",
    "linkerd-proxy",
    "linkerd-init",
    "linkerd-network-validator",
    "envoy-sidecar",
    "consul-dataplane",
    "consul-connect-inject-init",
];

/// Pod-spec keys about where a pod is scheduled.
const SCHEDULING: &[&str] = &[
    "affinity",
    "nodeSelector",
    "tolerations",
    "topologySpreadConstraints",
    "nodeName",
    "priorityClassName",
    "priority",
    "schedulerName",
    "runtimeClassName",
    "overhead",
    "preemptionPolicy",
];

type R<T> = Result<T, ComposeError>;

fn err<T>(m: impl Into<String>) -> R<T> {
    Err(ComposeError(m.into()))
}

// ---------------------------------------------------------------- reading the manifests

/// One object of the manifests.
#[derive(Clone, Debug)]
struct Obj {
    kind: String,
    api: String,
    name: String,
    ns: String,
    y: Y,
    /// where it came from: a file, or the `# Source:` line of `helm template`
    src: String,
}

/// The documents of a stream with their `# Source:` lines (helm template writes one above each
/// document). The stream is cut at the lines `---`, and each part is read on its own.
fn documents(text: &str, file: &str) -> R<Vec<(Y, String)>> {
    let mut parts: Vec<String> = vec![String::new()];
    for ln in text.split_inclusive('\n') {
        let t = ln.trim_end();
        if t == "---" || t.starts_with("--- ") || t == "..." {
            parts.push(String::new());
            continue;
        }
        parts.last_mut().unwrap().push_str(ln);
    }
    let mut out = Vec::new();
    for p in parts {
        if p.trim().is_empty() {
            continue;
        }
        let src = p
            .lines()
            .find_map(|l| l.trim().strip_prefix("# Source: ").map(str::to_string))
            .unwrap_or_else(|| file.to_string());
        for d in yaml::safe_load_all(&p).map_err(|e| ComposeError(format!("{file}: {e}")))? {
            if !d.is_none() {
                out.push((d, src.clone()));
            }
        }
    }
    Ok(out)
}

/// The objects of a text (YAML or JSON); `kind: List` is opened.
fn objects(text: &str, file: &str, ns: &str) -> R<Vec<Obj>> {
    let docs = if file.ends_with(".json") {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| ComposeError(format!("{file}: {e}")))?;
        vec![(crate::pyval::from_json(&v), file.to_string())]
    } else {
        documents(text, file)?
    };
    let mut out = Vec::new();
    let mut todo: Vec<(Y, String)> = docs;
    todo.reverse();
    while let Some((d, src)) = todo.pop() {
        if !d.is_map() {
            return err(format!(
                "{file}: a document is not a mapping (is this a Kubernetes manifest?)"
            ));
        }
        let kind = pstr(d.getn("kind"));
        if kind.ends_with("List") && d.getn("items").is_seq() {
            let mut items = as_list(d.get("items"));
            items.reverse();
            for i in items {
                todo.push((i, src.clone()));
            }
            continue;
        }
        if kind.is_empty() || d.getn("kind").is_none() {
            return err(format!(
                "{file}: a document has no kind: (is this a Kubernetes manifest?)"
            ));
        }
        let md = d.getn("metadata");
        let name = pstr(md.getn("name"));
        let name = if md.getn("name").is_none() {
            pstr(md.getn("generateName"))
                .trim_end_matches('-')
                .to_string()
        } else {
            name
        };
        let ons = md.getn("namespace");
        out.push(Obj {
            kind,
            api: pstr(d.getn("apiVersion")),
            name,
            ns: if ons.truthy() {
                pstr(ons)
            } else {
                ns.to_string()
            },
            y: d.clone(),
            src,
        });
    }
    Ok(out)
}

/// The manifest files under a folder, in name order.
fn manifest_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut es: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    es.sort();
    for p in es {
        let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if n.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            manifest_files(&p, out)?;
        } else if (n.ends_with(".yaml") || n.ends_with(".yml") || n.ends_with(".json"))
            && !matches!(
                n,
                "kustomization.yaml" | "kustomization.yml" | "Chart.yaml" | "values.yaml"
            )
        {
            out.push(p);
        }
    }
    Ok(())
}

/// What a path is: a Helm chart, a Kustomize folder, or manifests.
pub fn kind_of_folder(dir: &Path) -> &'static str {
    if dir.join("Chart.yaml").is_file() {
        "helm"
    } else if ["kustomization.yaml", "kustomization.yml", "Kustomization"]
        .iter()
        .any(|k| dir.join(k).is_file())
    {
        "kustomize"
    } else {
        "manifests"
    }
}

/// Run a renderer and return what it printed.
fn render(argv: &[String], what: &str, how: &str) -> R<String> {
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| {
            ComposeError(format!(
                "{what}: {} could not run ({e}). Install it, or render the manifests yourself and import the result: {how}",
                argv[0]
            ))
        })?;
    if !out.status.success() {
        return err(format!(
            "{what}: {} failed:\n{}",
            shown_command(argv),
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    String::from_utf8(out.stdout)
        .map_err(|_| ComposeError(format!("{what}: the output is not UTF-8")))
}

/// A renderer's command line with the values of `--set` left out (they may hold passwords).
fn shown_command(argv: &[String]) -> String {
    let mut out = Vec::new();
    let mut hide = false;
    for a in argv {
        if hide {
            out.push(match a.split_once('=') {
                Some((k, _)) => format!("{k}=..."),
                None => "...".into(),
            });
            hide = false;
            continue;
        }
        hide = matches!(a.as_str(), "--set" | "--set-string" | "--set-file");
        out.push(crate::shlex::quote(a));
    }
    out.join(" ")
}

// ---------------------------------------------------------------- small helpers

/// A value as text, and an absent value (`~`, a missing key) as "": a manifest's missing field
/// must never read as Python's "None" (a missing `clusterIP` is not a headless Service).
fn pstr(v: &Y) -> String {
    if v.is_none() {
        String::new()
    } else {
        crate::pyval::pstr(v)
    }
}

fn s(v: &str) -> Y {
    Y::s(v)
}

fn map(pairs: Vec<(&str, Y)>) -> Y {
    Y::Map(pairs.into_iter().map(|(k, v)| (s(k), v)).collect())
}

fn strs(v: &[String]) -> Y {
    Y::Seq(v.iter().map(|x| s(x)).collect())
}

/// Compose unescapes `$$` in commands, environment values and healthchecks.
fn esc(v: &str) -> String {
    v.replace('$', "$$")
}

/// `$(VAR)` of Kubernetes: the value of an earlier variable; `$$(VAR)` is a literal `$(VAR)`;
/// an unknown variable stays as written. Also returns the names that were filled in.
fn expand(v: &str, env: &[(String, String)]) -> (String, Vec<String>) {
    let b = v.as_bytes();
    let mut out = String::new();
    let mut used = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'$' {
            out.push('$');
            i += 2;
            continue;
        }
        if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'(' {
            if let Some(j) = v[i + 2..].find(')') {
                let name = &v[i + 2..i + 2 + j];
                if let Some((_, val)) = env.iter().rev().find(|(k, _)| k == name) {
                    out.push_str(val);
                    used.push(name.to_string());
                    i += 3 + j;
                    continue;
                }
            }
        }
        let c = v[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    (out, used)
}

/// A Kubernetes memory quantity in MiB (rounded up).
fn quantity_mib(v: &Y) -> Option<i128> {
    let t = pstr(v);
    let t = t.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (n, u) = t.split_at(split);
    let n: f64 = n.parse().ok()?;
    let f: f64 = match u {
        "Ki" => 1024.0,
        "Mi" => 1024.0 * 1024.0,
        "Gi" => 1024.0 * 1024.0 * 1024.0,
        "Ti" => 1024.0f64.powi(4),
        "k" | "K" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "m" => 1e-3,
        _ => return None,
    };
    Some(((n * f) / (1024.0 * 1024.0)).ceil() as i128)
}

fn quantity_mib_any(v: &Y) -> Option<i128> {
    match v {
        Y::Int(i) => Some((*i + (1 << 20) - 1) >> 20),
        _ => quantity_mib(v),
    }
}

/// Standard base64, as Secrets' `data:` holds it.
fn base64_decode(t: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in t.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn short_hash(t: &str) -> String {
    let h = Sha256::digest(t.as_bytes());
    h.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// A relative path inside a volume (an item's path): no `..`, not absolute.
fn safe_rel(p: &str) -> bool {
    !p.is_empty() && !p.starts_with('/') && !p.split('/').any(|c| c == ".." || c.is_empty())
}

/// The label selector `sel` (matchLabels or a plain mapping) matches `labels`.
fn selects(sel: &Y, labels: &Y) -> bool {
    let ml = if sel.has("matchLabels") || sel.has("matchExpressions") {
        sel.getn("matchLabels").clone()
    } else {
        sel.clone()
    };
    if !ml.truthy() {
        return false;
    }
    ml.map_items()
        .iter()
        .all(|(k, v)| labels.get(&pstr(k)).is_some_and(|x| pstr(x) == pstr(v)))
}

// ---------------------------------------------------------------- the model

/// One pod of a workload.
#[derive(Clone, Debug)]
struct Inst {
    pod: String,
    kind: String,
    workload: String,
    ns: String,
    src: String,
    ordinal: Option<usize>,
    replicas: usize,
    spec: Y,
    labels: Y,
    /// StatefulSet volumeClaimTemplates' names
    claims: Vec<String>,
    /// StatefulSet spec.serviceName
    sts_service: Option<String>,
    /// one-shot: "init" (a pre-install hook), "provision", "cron"
    job: Option<String>,
    schedule: Option<String>,
    parallel: bool,
}

/// One container that becomes a service.
#[derive(Clone, Debug)]
struct Ctr {
    /// the Compose-shaped service name
    name: String,
    inst: usize,
    c: Y,
    /// "app", "sidecar" or "init"
    role: &'static str,
}

/// A file the import writes next to the output (a ConfigMap's or a Secret's key).
#[derive(Clone, Debug)]
pub struct OutFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub mode: u32,
}

/// What the conversion gives: the document and the files to write before the import.
#[derive(Debug, Default)]
pub struct Converted {
    pub doc: Y,
    pub warnings: Vec<(String, String)>,
    pub notes: Vec<String>,
    pub files: Vec<OutFile>,
    pub services: usize,
}

/// The conversion's settings.
#[derive(Clone, Debug)]
pub struct Conv {
    /// the folder of files next to the output (`<out dir>/k8s-import`)
    pub dir: PathBuf,
    pub namespace: String,
    /// how addresses are given, as in hydo compose
    pub bridge: bool,
    pub no_addr: bool,
}

struct Cx<'a> {
    cv: &'a Conv,
    objs: Vec<Obj>,
    insts: Vec<Inst>,
    ctrs: Vec<Ctr>,
    w: Vec<(String, String)>,
    notes: Vec<String>,
    files: Vec<OutFile>,
    written: BTreeSet<PathBuf>,
    multi_ns: bool,
    /// per container service: its names in /etc/hosts
    aliases: Vec<Vec<String>>,
    /// per container service: the ports Services send to
    targeted: Vec<Vec<i128>>,
    /// the Services with a cluster address, for the variables Kubernetes gives every container
    /// (`<NAME>_SERVICE_HOST`, `_SERVICE_PORT`, `_SERVICE_PORT_<PORT NAME>`): namespace, name, the
    /// container service it resolves to, and (port name, the container's port)
    svc_env: Vec<SvcEnv>,
}

/// A Service's variables: namespace, name, the container service its name resolves to, and
/// (port name, the container's port).
type SvcEnv = (String, String, usize, Vec<(String, i128)>);

impl<'a> Cx<'a> {
    fn warn(&mut self, key: &str, t: String) {
        if !self.w.iter().any(|(k, x)| k == key && *x == t) {
            self.w.push((key.into(), t));
        }
    }

    fn note(&mut self, t: String) {
        if !self.notes.contains(&t) {
            self.notes.push(t);
        }
    }

    fn find(&self, kind: &str, ns: &str, name: &str) -> Option<&Obj> {
        self.objs
            .iter()
            .find(|o| o.kind == kind && o.ns == ns && o.name == name)
    }

    /// A ConfigMap's keys and contents (data, then binaryData).
    fn configmap(&self, ns: &str, name: &str) -> Option<Vec<(String, Vec<u8>)>> {
        let o = self.find("ConfigMap", ns, name)?;
        let mut out: Vec<(String, Vec<u8>)> =
            o.y.getn("data")
                .map_items()
                .iter()
                .map(|(k, v)| (pstr(k), pstr(v).into_bytes()))
                .collect();
        for (k, v) in o.y.getn("binaryData").map_items() {
            out.push((pstr(k), base64_decode(&pstr(v)).unwrap_or_default()));
        }
        Some(out)
    }

    /// A Secret's keys and values (data decoded, then stringData, which wins).
    fn secret(&self, ns: &str, name: &str) -> Option<Vec<(String, Vec<u8>)>> {
        let o = self.find("Secret", ns, name)?;
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        for (k, v) in o.y.getn("data").map_items() {
            out.push((pstr(k), base64_decode(&pstr(v)).unwrap_or_default()));
        }
        for (k, v) in o.y.getn("stringData").map_items() {
            let k = pstr(k);
            out.retain(|(x, _)| *x != k);
            out.push((k, pstr(v).into_bytes()));
        }
        Some(out)
    }

    fn add_file(&mut self, path: PathBuf, bytes: Vec<u8>, mode: u32) {
        if self.written.insert(path.clone()) {
            self.files.push(OutFile { path, bytes, mode });
        }
    }
}

// ---------------------------------------------------------------- the conversion

/// Convert the objects of the manifests. `objs` come from [`read_inputs`].
pub fn convert(texts: &[(String, String)], cv: &Conv) -> R<Converted> {
    let mut objs = Vec::new();
    for (file, text) in texts {
        objs.extend(objects(text, file, &cv.namespace)?);
    }
    let mut cx = Cx {
        cv,
        objs,
        insts: Vec::new(),
        ctrs: Vec::new(),
        w: Vec::new(),
        notes: Vec::new(),
        files: Vec::new(),
        written: BTreeSet::new(),
        multi_ns: false,
        aliases: Vec::new(),
        targeted: Vec::new(),
        svc_env: Vec::new(),
    };
    cx.kinds();
    cx.expand_workloads()?;
    if cx.insts.is_empty() {
        return err("no workload to import: the manifests have no Deployment, StatefulSet, DaemonSet, ReplicaSet, Job, CronJob or Pod");
    }
    let nss: BTreeSet<&str> = cx.insts.iter().map(|i| i.ns.as_str()).collect();
    cx.multi_ns = nss.len() > 1;
    cx.containers();
    cx.aliases = cx
        .ctrs
        .iter()
        .map(|c| {
            if c.role == "app"
                && cx
                    .ctrs
                    .iter()
                    .find(|x| x.inst == c.inst && x.role == "app")
                    .map(|x| &x.name)
                    == Some(&c.name)
            {
                vec![cx.insts[c.inst].pod.clone()]
            } else {
                Vec::new()
            }
        })
        .collect();
    cx.targeted = vec![Vec::new(); cx.ctrs.len()];
    cx.services();
    let doc = cx.document()?;
    let services = cx.ctrs.len();
    Ok(Converted {
        doc,
        warnings: cx.w,
        notes: cx.notes,
        files: cx.files,
        services,
    })
}

impl<'a> Cx<'a> {
    /// One line per kind that is not a workload, a ConfigMap, a Secret, a Service or a claim.
    fn kinds(&mut self) {
        let mut seen: Vec<String> = Vec::new();
        let objs = self.objs.clone();
        for o in &objs {
            let k = o.kind.as_str();
            if WORKLOADS.contains(&k)
                || matches!(
                    k,
                    "ConfigMap" | "Secret" | "Service" | "PersistentVolumeClaim"
                )
            {
                continue;
            }
            let names: Vec<String> = objs
                .iter()
                .filter(|x| x.kind == o.kind)
                .map(|x| x.name.clone())
                .collect();
            if seen.contains(&o.kind) {
                continue;
            }
            seen.push(o.kind.clone());
            let list = names.join(", ");
            if let Some((_, why)) = QUIET.iter().find(|(q, _)| *q == k) {
                self.note(format!("{k} {list}: ignored; {why}"));
            } else if let Some((_, why)) = REFUSED.iter().find(|(q, _)| *q == k) {
                self.warn(k, format!("{k} {list}: not imported; {why}"));
            } else {
                self.warn(
                    "custom",
                    format!(
                        "{k} {list} (apiVersion {}): a custom resource; its operator does not run in hydo, so it is not imported (import the workloads the operator would create instead)",
                        o.api
                    ),
                );
            }
        }
    }

    fn expand_workloads(&mut self) -> R<()> {
        let objs = self.objs.clone();
        // Helm's pre-install hooks first: the pods wait for them
        let mut order: Vec<&Obj> = objs
            .iter()
            .filter(|o| WORKLOADS.contains(&o.kind.as_str()))
            .collect();
        let pre = |o: &Obj| {
            hooks(o)
                .iter()
                .any(|h| h == "pre-install" || h == "pre-upgrade")
        };
        order.sort_by_key(|o| !pre(o));
        for o in order {
            let hk = hooks(o);
            let wh = format!("{} {}", o.kind, o.name);
            if hk.iter().any(|h| h.starts_with("test")) {
                self.note(format!(
                    "{wh}: a Helm test, left out; its command can become a check (hydo/checks/)"
                ));
                continue;
            }
            if !hk.is_empty()
                && !hk
                    .iter()
                    .any(|h| h.ends_with("-install") || h.ends_with("-upgrade"))
            {
                self.note(format!("{wh}: a Helm hook ({}), left out: hydo installs once and never deletes or rolls back", hk.join(",")));
                continue;
            }
            let spec = o.y.getn("spec");
            let (tmpl, job, schedule) = match o.kind.as_str() {
                "Pod" => (o.y.clone(), None, None),
                "CronJob" => (
                    spec.getn("jobTemplate")
                        .getn("spec")
                        .getn("template")
                        .clone(),
                    Some("cron".to_string()),
                    Some(pstr(spec.getn("schedule"))),
                ),
                "Job" => (
                    spec.getn("template").clone(),
                    Some("provision".to_string()),
                    None,
                ),
                _ => (spec.getn("template").clone(), None, None),
            };
            let job = if hk.iter().any(|h| h.starts_with("pre-")) {
                Some("init".to_string())
            } else if !hk.is_empty() {
                Some("provision".to_string())
            } else {
                job
            };
            if !tmpl.getn("spec").is_map() {
                self.warn("workload", format!("{wh}: no pod template; not imported"));
                continue;
            }
            let labels = tmpl.getn("metadata").getn("labels").clone();
            let replicas = match spec.get("replicas") {
                Some(v) if !v.is_none() => int_of(v).unwrap_or(1).max(0) as usize,
                _ => 1,
            };
            let base = Inst {
                pod: o.name.clone(),
                kind: o.kind.clone(),
                workload: o.name.clone(),
                ns: o.ns.clone(),
                src: o.src.clone(),
                ordinal: None,
                replicas,
                spec: tmpl.getn("spec").clone(),
                labels,
                claims: Vec::new(),
                sts_service: None,
                job,
                schedule,
                parallel: false,
            };
            match o.kind.as_str() {
                "StatefulSet" => {
                    if replicas == 0 {
                        self.note(format!("{wh}: replicas: 0, so no pod; not imported"));
                        continue;
                    }
                    let claims: Vec<String> = as_list(spec.get("volumeClaimTemplates"))
                        .iter()
                        .map(|c| pstr(c.getn("metadata").getn("name")))
                        .collect();
                    let svc = spec.get("serviceName").filter(|v| v.truthy()).map(pstr);
                    let parallel = pstr(spec.getn("podManagementPolicy")) == "Parallel";
                    for i in 0..replicas {
                        let mut x = base.clone();
                        x.pod = format!("{}-{i}", o.name);
                        x.ordinal = Some(i);
                        x.claims = claims.clone();
                        x.sts_service = svc.clone();
                        x.parallel = parallel;
                        self.insts.push(x);
                    }
                }
                "Deployment" | "ReplicaSet" | "ReplicationController" => {
                    if replicas == 0 {
                        self.note(format!("{wh}: replicas: 0, so no pod; not imported"));
                        continue;
                    }
                    if replicas > 1 {
                        self.warn("replicas", format!("{wh}: replicas: {replicas}: one pod is imported (the pods of a {} are alike, and a Service name resolves to one address in hydo, without load balancing); copy the service for more", o.kind));
                    }
                    self.insts.push(base);
                }
                "DaemonSet" => {
                    self.note(format!("{wh}: one pod (hydo is one machine)"));
                    self.insts.push(base);
                }
                _ => self.insts.push(base),
            }
        }
        Ok(())
    }

    /// The containers of every pod, in order: the init containers (one-shot jobs), then the
    /// containers; a service mesh's containers are left out.
    fn containers(&mut self) {
        let insts = self.insts.clone();
        for (ix, i) in insts.iter().enumerate() {
            let base = if self.multi_ns {
                format!("{}-{}", i.ns, i.pod)
            } else {
                i.pod.clone()
            };
            let mut inits = Vec::new();
            let mut apps = Vec::new();
            for c in as_list(i.spec.get("initContainers")) {
                let n = pstr(c.getn("name"));
                if MESH.contains(&n.as_str()) {
                    self.warn("mesh", format!("pod {}: container {n} belongs to a service mesh and is left out (no mesh runs in hydo; services talk directly)", i.pod));
                } else if pstr(c.getn("restartPolicy")) == "Always" {
                    apps.push((c.clone(), "sidecar"));
                } else {
                    inits.push(c.clone());
                }
            }
            for c in as_list(i.spec.get("containers")) {
                let n = pstr(c.getn("name"));
                if MESH.contains(&n.as_str()) {
                    self.warn("mesh", format!("pod {}: container {n} belongs to a service mesh and is left out (no mesh runs in hydo; services talk directly)", i.pod));
                } else {
                    apps.push((c.clone(), "app"));
                }
            }
            // the first app container is the pod's own service
            apps.sort_by_key(|(_, r)| *r == "sidecar");
            if apps.is_empty() {
                continue;
            }
            for c in inits {
                let n = pstr(c.getn("name"));
                self.ctrs.push(Ctr {
                    name: format!("{base}-init-{n}"),
                    inst: ix,
                    c,
                    role: "init",
                });
            }
            let napps = apps.len();
            for (k, (c, role)) in apps.into_iter().enumerate() {
                let n = pstr(c.getn("name"));
                let name = if k == 0 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                };
                self.ctrs.push(Ctr {
                    name,
                    inst: ix,
                    c,
                    role,
                });
            }
            if napps > 1 && i.job.is_none() {
                let names: Vec<String> = self
                    .ctrs
                    .iter()
                    .filter(|c| c.inst == ix && c.role != "init")
                    .map(|c| pstr(c.c.getn("name")))
                    .collect();
                self.warn("pod", format!("pod {}: its containers {} share localhost in Kubernetes; in hydo each is its own service with its own address, so a container that reaches another as localhost:<port> must name it instead (or import with --network loopback, where every service shares one localhost)", i.pod, names.join(", ")));
            }
        }
    }

    /// The port numbers of a container, by name too.
    fn port_of(c: &Y, p: &Y) -> Option<i128> {
        if let Some(n) = int_of(p).filter(|_| !matches!(p, Y::Str(x) if x.parse::<i128>().is_err()))
        {
            return Some(n);
        }
        let name = pstr(p);
        as_list(c.get("ports"))
            .iter()
            .find(|x| pstr(x.getn("name")) == name)
            .and_then(|x| int_of(x.getn("containerPort")))
    }

    /// Services: names in /etc/hosts, and the ports they send to.
    fn services(&mut self) {
        let objs: Vec<Obj> = self
            .objs
            .iter()
            .filter(|o| o.kind == "Service")
            .cloned()
            .collect();
        for o in objs {
            let sp = o.y.getn("spec").clone();
            let wh = format!("Service {}", o.name);
            let typ = pstr(sp.getn("type"));
            if typ == "ExternalName" {
                self.warn("service", format!("{wh} is an ExternalName ({}): a system outside the manifests; its name does not resolve (make it a fake, fakes:, or leave it out)", pstr(sp.getn("externalName"))));
                continue;
            }
            let sel = sp.getn("selector").clone();
            if !sel.truthy() {
                self.warn("service", format!("{wh} has no selector (its endpoints are kept outside the manifests): its name does not resolve"));
                continue;
            }
            let pods: Vec<usize> = self
                .insts
                .iter()
                .enumerate()
                .filter(|(_, i)| i.ns == o.ns && i.job.is_none() && selects(&sel, &i.labels))
                .map(|(k, _)| k)
                .collect();
            if pods.is_empty() {
                self.warn(
                    "service",
                    format!("{wh} selects no pod of the manifests; its name does not resolve"),
                );
                continue;
            }
            if matches!(typ.as_str(), "NodePort" | "LoadBalancer") {
                self.note(format!("{wh} is a {typ}: there are no nodes or load balancers in hydo; clients in the guest use the name {}", o.name));
            }
            let headless = pstr(sp.getn("clusterIP")) == "None";
            let ports = as_list(sp.get("ports"));
            // the container that serves the Service: the first whose port is a target
            let target_of = |cx: &Cx, inst: usize| -> (usize, Vec<i128>) {
                let cs: Vec<usize> = cx
                    .ctrs
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.inst == inst && c.role != "init")
                    .map(|(k, _)| k)
                    .collect();
                for &k in &cs {
                    let c = &cx.ctrs[k].c;
                    let ts: Vec<i128> = ports
                        .iter()
                        .filter_map(|p| {
                            let t = p
                                .get("targetPort")
                                .filter(|t| !t.is_none())
                                .unwrap_or(p.getn("port"));
                            Self::port_of(c, t)
                        })
                        .filter(|t| {
                            as_list(c.get("ports"))
                                .iter()
                                .any(|x| int_of(x.getn("containerPort")) == Some(*t))
                        })
                        .collect();
                    if !ts.is_empty() {
                        return (k, ts);
                    }
                }
                (cs.first().copied().unwrap_or(0), Vec::new())
            };
            let ns = o.ns.clone();
            let names = |n: &str| -> Vec<String> {
                vec![
                    n.to_string(),
                    format!("{n}.{ns}"),
                    format!("{n}.{ns}.svc"),
                    format!("{n}.{ns}.svc.cluster.local"),
                ]
            };
            let (first, ts) = target_of(self, pods[0]);
            if self.ctrs.iter().all(|c| c.inst != pods[0]) {
                continue;
            }
            for n in names(&o.name) {
                if !self.aliases[first].contains(&n) {
                    self.aliases[first].push(n);
                }
            }
            for &k in &pods {
                let (c, ts) = target_of(self, k);
                for t in ts {
                    if !self.targeted[c].contains(&t) {
                        self.targeted[c].push(t);
                    }
                }
                if headless && self.insts[k].sts_service.as_deref() == Some(o.name.as_str()) {
                    let pod = self.insts[k].pod.clone();
                    for n in names(&format!("{pod}.{}", o.name)) {
                        if !self.aliases[c].contains(&n) {
                            self.aliases[c].push(n);
                        }
                    }
                }
            }
            if pods.len() > 1 {
                let list: Vec<String> = pods.iter().map(|k| self.insts[*k].pod.clone()).collect();
                self.note(format!("{wh} selects {} pods ({}): the name {} resolves to the first; there is no load balancing in hydo", pods.len(), list.join(", "), o.name));
            }
            let c0 = self.ctrs[first].c.clone();
            if !headless {
                let ps: Vec<(String, i128)> = ports
                    .iter()
                    .filter_map(|p| {
                        let t = p
                            .get("targetPort")
                            .filter(|t| !t.is_none())
                            .unwrap_or(p.getn("port"));
                        Self::port_of(&c0, t).map(|n| (pstr(p.getn("name")), n))
                    })
                    .collect();
                self.svc_env.push((o.ns.clone(), o.name.clone(), first, ps));
            }
            for p in &ports {
                let port = int_of(p.getn("port"));
                let t = p
                    .get("targetPort")
                    .filter(|t| !t.is_none())
                    .unwrap_or(p.getn("port"));
                let tn = Self::port_of(&c0, t);
                if let (Some(a), Some(b)) = (port, tn) {
                    if a != b {
                        self.warn("service-port", format!("{wh}: port {a} goes to the container's port {b}; hydo has no proxy between them, so a client must use {}:{b} (or make the program listen on {a})", o.name));
                    }
                }
            }
            let _ = ts;
        }
    }

    /// The document in Compose's shape.
    fn document(&mut self) -> R<Y> {
        let mut svcs: Vec<(Y, Y)> = Vec::new();
        let mut vols: Vec<String> = Vec::new();
        let mut perms: Vec<Vec<(String, String)>> = Vec::new();
        let mut env_texts: Vec<Vec<String>> = Vec::new();
        for k in 0..self.ctrs.len() {
            let (y, v, p, t) = self.service(k)?;
            vols.extend(v);
            perms.push(p);
            env_texts.push(t);
            svcs.push((s(&self.ctrs[k].name.clone()), y));
        }
        // the volumes on /data, as the Compose importer decides it: several services with their
        // own volume on /data, or one beside other volumes, get each their own directory
        let mut on_data: BTreeSet<String> = BTreeSet::new();
        let mut elsewhere = false;
        for (_, y) in &svcs {
            for v in as_list(y.get("volumes")) {
                if pstr(v.getn("type")) != "volume" {
                    continue;
                }
                if pstr(v.getn("target")).trim_end_matches('/') == "/data" {
                    on_data.insert(safe_name(&pstr(v.getn("source"))));
                } else {
                    elsewhere = true;
                }
            }
        }
        let own_data = on_data.len() > 1 || (!on_data.is_empty() && elsewhere);
        // dependencies between pods: the names a container's environment, command and mounted
        // configuration name
        let mut by_alias: Vec<(String, usize)> = Vec::new();
        for (k, al) in self.aliases.iter().enumerate() {
            for a in al {
                by_alias.push((a.clone(), k));
            }
        }
        let mut deps: Vec<Vec<(usize, String)>> = vec![Vec::new(); self.ctrs.len()];
        for k in 0..self.ctrs.len() {
            if self.ctrs[k].role == "init" {
                continue;
            }
            let wk = self.insts[self.ctrs[k].inst].workload.clone();
            for t in &env_texts[k] {
                for tok in t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
                    let tok = tok.trim_matches('.');
                    if let Some((_, d)) = by_alias.iter().find(|(a, _)| a == tok) {
                        let d = *d;
                        if d == k
                            || self.insts[self.ctrs[d].inst].workload == wk
                            || deps[k].iter().any(|(x, _)| *x == d)
                        {
                            continue;
                        }
                        if reaches(&deps, d, k) {
                            self.note(format!("{} names {tok}, and {tok} (or a service it waits for) names {}: no start order between them is assumed", self.ctrs[k].name, self.ctrs[k].name));
                            continue;
                        }
                        deps[k].push((d, tok.to_string()));
                    }
                }
            }
        }
        // the groups: the pods of a StatefulSet with several replicas
        let mut groups: Vec<Y> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for i in &self.insts {
            if i.kind != "StatefulSet" || i.replicas < 2 || seen.contains(&i.workload) {
                continue;
            }
            seen.push(i.workload.clone());
            let members: Vec<String> = self
                .ctrs
                .iter()
                .filter(|c| {
                    c.role == "app"
                        && self.insts[c.inst].workload == i.workload
                        && self.insts[c.inst].kind == "StatefulSet"
                        && self
                            .ctrs
                            .iter()
                            .find(|x| x.inst == c.inst && x.role == "app")
                            .map(|x| &x.name)
                            == Some(&c.name)
                })
                .map(|c| c.name.clone())
                .collect();
            let gname = if self.multi_ns {
                format!("{}-{}", i.ns, i.workload)
            } else {
                i.workload.clone()
            };
            groups.push(map(vec![
                ("name", s(&gname)),
                ("services", strs(&members)),
                (
                    "description",
                    s(&format!(
                        "StatefulSet {}: its {} pods",
                        i.workload, i.replicas
                    )),
                ),
            ]));
        }
        // the services, with their dependencies, setup and names
        let mut out: Vec<(Y, Y)> = Vec::new();
        for (k, (n, mut y)) in svcs.into_iter().enumerate() {
            let ctr = self.ctrs[k].clone();
            let inst = self.insts[ctr.inst].clone();
            let mut dep: Vec<(Y, Y)> = Vec::new();
            let mut comments = as_list(y.get("x-hydo-comments"));
            // the pod's init containers, then Helm's pre-install hooks
            if ctr.role != "init" && inst.job.is_none() {
                for (j, c) in self.ctrs.iter().enumerate() {
                    if c.inst == ctr.inst && c.role == "init" && j != k {
                        dep.push((
                            s(&c.name),
                            map(vec![("condition", s("service_completed_successfully"))]),
                        ));
                    }
                }
                for (j, c) in self.ctrs.iter().enumerate() {
                    if self.insts[c.inst].job.as_deref() == Some("init")
                        && c.role == "app"
                        && j != k
                    {
                        dep.push((
                            s(&c.name),
                            map(vec![("condition", s("service_completed_successfully"))]),
                        ));
                    }
                }
            }
            for (d, why) in &deps[k] {
                dep.push((
                    s(&self.ctrs[*d].name),
                    map(vec![("condition", s("service_healthy"))]),
                ));
                comments.push(s(&format!("depends_on {}: its environment, command or configuration names {why}; hydo starts it once {} is ready", self.ctrs[*d].name, self.ctrs[*d].name)));
            }
            if ctr.role == "init" {
                // an init container waits for the one before it
                if let Some(prev) = self.ctrs[..k]
                    .iter()
                    .rev()
                    .find(|c| c.inst == ctr.inst && c.role == "init")
                {
                    dep.push((
                        s(&prev.name),
                        map(vec![("condition", s("service_completed_successfully"))]),
                    ));
                }
            }
            if !dep.is_empty() {
                y.set(s("depends_on"), Y::Map(dep));
            }
            // a StatefulSet's pods start in order, each once the one before it is ready
            // (OrderedReady); Parallel starts them together
            if inst.parallel && inst.ordinal.is_some_and(|o| o > 0) && ctr.role == "app" {
                y.set(
                    s("x-hydo-start-after"),
                    s(&format!(
                        "the StatefulSet {} has podManagementPolicy: Parallel",
                        inst.workload
                    )),
                );
            }
            // the setup: the volumes' modes and group, as Kubernetes gives them
            let fs_group = int_of(inst.spec.getn("securityContext").getn("fsGroup"));
            let policy = pstr(
                inst.spec
                    .getn("securityContext")
                    .getn("fsGroupChangePolicy"),
            );
            let mut setup: Vec<String> = Vec::new();
            for (vol, what) in &perms[k] {
                let d = if !own_data && on_data.contains(vol) && what.ends_with("/data") {
                    "/data".to_string()
                } else {
                    format!("/data/vol/{vol}")
                };
                if what.starts_with("emptyDir")
                    || what.starts_with("claim")
                    || what.starts_with("hostPath")
                {
                    setup.push(format!("{{ [ -n \"$(ls -A {d})\" ] || chmod 0777 {d}; }}"));
                }
                if let Some(g) = fs_group {
                    if policy == "OnRootMismatch" {
                        setup.push(format!("{{ [ \"$(stat -c %g {d})\" = {g} ] || {{ chgrp -R {g} {d} && chmod -R g+rwX {d}; }}; }}"));
                    } else {
                        setup.push(format!("chgrp -R {g} {d} && chmod -R g+rwX {d}"));
                    }
                }
            }
            if !setup.is_empty() {
                let mut pre = vec![format!(
                    "mkdir -p {}",
                    perms[k]
                        .iter()
                        .map(
                            |(v, w)| if !own_data && on_data.contains(v) && w.ends_with("/data") {
                                "/data".to_string()
                            } else {
                                format!("/data/vol/{v}")
                            }
                        )
                        .collect::<Vec<_>>()
                        .join(" ")
                )];
                pre.extend(setup);
                y.set(s("x-hydo-setup"), s(&pre.join(" && ")));
                if fs_group.is_some() {
                    comments.push(s(&format!("setup: the volumes become group {}'s (securityContext.fsGroup), as Kubernetes does; an empty claim or emptyDir is mode 0777 at first", fs_group.unwrap_or(0))));
                } else {
                    comments.push(s("setup: an empty claim or emptyDir is mode 0777 at first, as most Kubernetes volume provisioners make it"));
                }
            }
            if !comments.is_empty() {
                y.set(s("x-hydo-comments"), Y::Seq(comments));
            }
            let al = self.aliases[k].clone();
            if !al.is_empty() {
                y.set(
                    s("networks"),
                    map(vec![("default", map(vec![("aliases", strs(&al))]))]),
                );
            }
            out.push((n, y));
        }
        let mut top = vec![(s("services"), Y::Map(out))];
        let mut vs: Vec<String> = Vec::new();
        for v in vols {
            if !vs.contains(&v) {
                vs.push(v);
            }
        }
        if !vs.is_empty() {
            top.push((
                s("volumes"),
                Y::Map(vs.iter().map(|v| (s(v), Y::Map(vec![]))).collect()),
            ));
        }
        if !groups.is_empty() {
            top.push((s("x-hydo-groups"), Y::Seq(groups)));
        }
        Ok(Y::Map(top))
    }

    /// One container as a Compose service: the service, its named volumes, its data-disk
    /// volumes (name, what) for the setup, and its texts that may name other pods.
    #[allow(clippy::type_complexity)]
    fn service(&mut self, k: usize) -> R<(Y, Vec<String>, Vec<(String, String)>, Vec<String>)> {
        let ctr = self.ctrs[k].clone();
        let inst = self.insts[ctr.inst].clone();
        let c = ctr.c.clone();
        let cn = pstr(c.getn("name"));
        let wh = format!("pod {}, container {cn}", inst.pod);
        let mut y: Vec<(Y, Y)> = Vec::new();
        let mut comments: Vec<String> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        let Some(image) = c.get("image").filter(|v| v.truthy()).map(pstr) else {
            return err(format!("{wh}: no image:"));
        };
        y.push((s("image"), s(&image)));
        // where it came from
        let what = match (inst.kind.as_str(), inst.ordinal) {
            ("StatefulSet", Some(o)) => format!(
                "Kubernetes StatefulSet {}, pod {} (replica {} of {})",
                inst.workload,
                inst.pod,
                o + 1,
                inst.replicas
            ),
            ("Pod", _) => format!("Kubernetes Pod {}", inst.pod),
            (kd, _) => format!("Kubernetes {kd} {}", inst.workload),
        };
        let role = match ctr.role {
            "init" => format!("init container {cn}"),
            "sidecar" => format!("sidecar container {cn}"),
            _ => format!("container {cn}"),
        };
        let mut origin = vec![format!("{what}, {role}")];
        if inst.ns != self.cv.namespace || self.multi_ns {
            origin.push(format!("namespace {}", inst.ns));
        }
        origin.push(format!("from {}", inst.src));
        y.push((s("x-hydo-origin"), strs(&origin)));
        // the environment: envFrom, then env; values from Secrets go to the env file
        let mut env: Vec<(String, String)> = Vec::new();
        let mut secret_keys: BTreeSet<String> = BTreeSet::new();
        let put = |env: &mut Vec<(String, String)>, k: String, v: String| {
            env.retain(|(x, _)| *x != k);
            env.push((k, v));
        };
        // what Kubernetes gives every container first: HOSTNAME (the pod's host name), and the
        // Services of its namespace (unless enableServiceLinks: false), with the port that works
        // in hydo: the container's, as there is no proxy
        let host = match inst.spec.get("hostname").filter(|v| v.truthy()) {
            Some(h) => pstr(h),
            None => inst.pod.clone(),
        };
        put(&mut env, "HOSTNAME".into(), host);
        if inst.spec.get("enableServiceLinks") != Some(&Y::Bool(false)) {
            for (ns, name, at, ps) in self.svc_env.clone() {
                if ns != inst.ns {
                    continue;
                }
                let v = name.to_uppercase().replace(['-', '.'], "_");
                put(&mut env, format!("{v}_SERVICE_HOST"), self.addr(at));
                if let Some((_, p)) = ps.first() {
                    put(&mut env, format!("{v}_SERVICE_PORT"), p.to_string());
                }
                for (pn, p) in ps.iter().filter(|(pn, _)| !pn.is_empty()) {
                    let pn = pn.to_uppercase().replace(['-', '.'], "_");
                    put(&mut env, format!("{v}_SERVICE_PORT_{pn}"), p.to_string());
                }
            }
        }
        for ef in as_list(c.get("envFrom")) {
            let prefix = pstr(ef.getn("prefix"));
            let prefix = if ef.getn("prefix").is_none() {
                String::new()
            } else {
                prefix
            };
            for (key, secret) in [("configMapRef", false), ("secretRef", true)] {
                let r = ef.getn(key);
                if !r.is_map() {
                    continue;
                }
                let name = pstr(r.getn("name"));
                let data = if secret {
                    self.secret(&inst.ns, &name)
                } else {
                    self.configmap(&inst.ns, &name)
                };
                match data {
                    Some(d) => {
                        for (dk, dv) in d {
                            let kk = format!("{prefix}{dk}");
                            secret_keys.remove(&kk);
                            if secret {
                                secret_keys.insert(kk.clone());
                            }
                            put(&mut env, kk, String::from_utf8_lossy(&dv).into_owned());
                        }
                    }
                    None => self.missing(
                        &wh,
                        if secret { "Secret" } else { "ConfigMap" },
                        &name,
                        r.getn("optional").truthy(),
                    ),
                }
            }
        }
        for e in as_list(c.get("env")) {
            let name = pstr(e.getn("name"));
            if let Some(v) = e.get("value").filter(|v| !v.is_none()) {
                let (val, used) = expand(&pstr(v), &env);
                secret_keys.remove(&name);
                if used.iter().any(|u| secret_keys.contains(u)) {
                    secret_keys.insert(name.clone());
                }
                put(&mut env, name, val);
                continue;
            }
            let vf = e.getn("valueFrom");
            if let Some(r) = vf.get("configMapKeyRef").filter(|r| r.is_map()) {
                let (cm, key) = (pstr(r.getn("name")), pstr(r.getn("key")));
                match self
                    .configmap(&inst.ns, &cm)
                    .and_then(|d| d.into_iter().find(|(x, _)| *x == key))
                {
                    Some((_, v)) => {
                        secret_keys.remove(&name);
                        put(&mut env, name, String::from_utf8_lossy(&v).into_owned())
                    }
                    None => self.missing(
                        &wh,
                        "ConfigMap",
                        &format!("{cm} (key {key}, variable {name})"),
                        r.getn("optional").truthy(),
                    ),
                }
            } else if let Some(r) = vf.get("secretKeyRef").filter(|r| r.is_map()) {
                let (sc, key) = (pstr(r.getn("name")), pstr(r.getn("key")));
                match self
                    .secret(&inst.ns, &sc)
                    .and_then(|d| d.into_iter().find(|(x, _)| *x == key))
                {
                    Some((_, v)) => {
                        secret_keys.insert(name.clone());
                        put(&mut env, name, String::from_utf8_lossy(&v).into_owned())
                    }
                    None => self.missing(
                        &wh,
                        "Secret",
                        &format!("{sc} (key {key}, variable {name})"),
                        r.getn("optional").truthy(),
                    ),
                }
            } else if let Some(r) = vf.get("fieldRef").filter(|r| r.is_map()) {
                let fp = pstr(r.getn("fieldPath"));
                let v = match fp.as_str() {
                    "metadata.name" => Some(inst.pod.clone()),
                    "metadata.namespace" => Some(inst.ns.clone()),
                    "metadata.uid" => Some(format!("hydo-{}", inst.pod)),
                    "spec.nodeName" => Some("hydo".to_string()),
                    "spec.serviceAccountName" => Some({
                        let sa = pstr(inst.spec.getn("serviceAccountName"));
                        if sa.is_empty() || inst.spec.getn("serviceAccountName").is_none() {
                            "default".into()
                        } else {
                            sa
                        }
                    }),
                    "status.podIP" | "status.podIPs" => Some(self.addr(k)),
                    "status.hostIP" | "status.hostIPs" => Some(if self.cv.bridge {
                        "10.89.0.1".into()
                    } else {
                        "127.0.0.1".into()
                    }),
                    _ => {
                        let lab = |pre: &str, m: &Y| -> Option<String> {
                            let key = fp
                                .strip_prefix(pre)?
                                .strip_prefix("['")?
                                .strip_suffix("']")?;
                            m.get(key).map(pstr)
                        };
                        lab("metadata.labels", &inst.labels)
                    }
                };
                match v {
                    Some(v) => put(&mut env, name, v),
                    None => self.warn(
                        "env",
                        format!(
                            "{wh}: variable {name} from fieldRef {fp}: not known in hydo; left out"
                        ),
                    ),
                }
            } else if let Some(r) = vf.get("resourceFieldRef").filter(|r| r.is_map()) {
                let res = pstr(r.getn("resource"));
                let (sect, what) = res.split_once('.').unwrap_or(("", ""));
                let q = c.getn("resources").getn(sect).getn(what).clone();
                let v = if what == "memory" {
                    quantity_mib(&q).map(|m| (m * 1024 * 1024).to_string())
                } else if what == "cpu" && q.truthy() {
                    let t = pstr(&q);
                    Some(match t.strip_suffix('m') {
                        Some(n) => n
                            .parse::<i128>()
                            .map(|n| ((n + 999) / 1000).max(1).to_string())
                            .unwrap_or(t),
                        None => t,
                    })
                } else {
                    None
                };
                match v {
                    Some(v) => put(&mut env, name, v),
                    None => self.warn("env", format!("{wh}: variable {name} from resourceFieldRef {res}: no such limit in the container; left out")),
                }
            } else {
                self.warn(
                    "env",
                    format!("{wh}: variable {name} has no value; left out"),
                );
            }
        }
        for (_, v) in &env {
            texts.push(v.clone());
        }
        let plain: Vec<(Y, Y)> = env
            .iter()
            .filter(|(k, _)| !secret_keys.contains(k))
            .map(|(k, v)| (s(k), s(&esc(v))))
            .collect();
        if !plain.is_empty() {
            y.push((s("environment"), Y::Map(plain)));
        }
        let secret_env: Vec<(String, String)> = env
            .iter()
            .filter(|(k, _)| secret_keys.contains(k))
            .cloned()
            .collect();
        if !secret_env.is_empty() {
            let mut text = String::from(
                "# values from Kubernetes Secrets, written by hydo k8s; never commit this file\n",
            );
            for (k, v) in &secret_env {
                if v.contains('\n') {
                    self.warn("secret", format!("{wh}: variable {k} from a Secret has several lines, which an env_file cannot hold; left out (mount the Secret as a file instead)"));
                    continue;
                }
                text.push_str(&format!("{k}={v}\n"));
            }
            let f = self
                .cv
                .dir
                .join("secrets")
                .join(format!("{}.env", safe_name(&ctr.name)));
            self.add_file(f.clone(), text.into_bytes(), 0o600);
            y.push((
                s("x-hydo-env-file"),
                s(&format!("k8s-import/secrets/{}.env", safe_name(&ctr.name))),
            ));
            let ks: Vec<&str> = secret_env.iter().map(|(k, _)| k.as_str()).collect();
            comments.push(format!("env_file: {} come from Secrets; their values are in k8s-import/secrets/, which version control ignores", ks.join(", ")));
        }
        // command and args, $(VAR) filled in from the environment
        for (key, to) in [("command", "entrypoint"), ("args", "command")] {
            let v = as_list(c.get(key));
            if v.is_empty() && c.get(key).is_none() {
                continue;
            }
            let mut words = Vec::new();
            for x in v {
                let (t, used) = expand(&pstr(&x), &env);
                if used.iter().any(|u| secret_keys.contains(u)) {
                    self.warn("secret", format!("{wh}: {key} names a variable from a Secret ($(...)); it is left unexpanded so that the value stays out of hydo.yaml"));
                    words.push(esc(&pstr(&x)));
                } else {
                    texts.push(t.clone());
                    words.push(esc(&t));
                }
            }
            y.push((s(to), strs(&words)));
        }
        if let Some(wd) = c.get("workingDir").filter(|v| v.truthy()) {
            y.push((s("working_dir"), s(&pstr(wd))));
        }
        // the user
        let psc = inst.spec.getn("securityContext");
        let csc = c.getn("securityContext");
        let pick = |k: &str| {
            csc.get(k)
                .filter(|v| !v.is_none())
                .or_else(|| psc.get(k).filter(|v| !v.is_none()))
                .and_then(int_of)
        };
        if let Some(u) = pick("runAsUser") {
            let user = match pick("runAsGroup") {
                Some(g) => format!("{u}:{g}"),
                None => u.to_string(),
            };
            y.push((s("user"), s(&user)));
        } else if let Some(g) = pick("runAsGroup") {
            self.warn("user", format!("{wh}: runAsGroup {g} without runAsUser: the image's user runs with its own group"));
        }
        if csc.getn("privileged").truthy() {
            self.warn("privileged", format!("{wh}: privileged: hydo runs every service as a plain process; remove it if the program does not need it"));
        }
        let add = as_list(csc.getn("capabilities").get("add"));
        if !add.is_empty() {
            self.note(format!("{wh}: capabilities.add {}: capabilities are not modelled (a program runs as root or as its user)", add.iter().map(pstr).collect::<Vec<_>>().join(", ")));
        }
        // ports: the ones a Service sends to first
        let mut ports: Vec<i128> = self.targeted[k].clone();
        for p in as_list(c.get("ports")) {
            if let Some(n) = int_of(p.getn("containerPort")) {
                if !ports.contains(&n) {
                    ports.push(n);
                }
                if p.getn("hostPort").truthy() {
                    self.note(format!("{wh}: hostPort {}: there is no host in hydo; the port is the service's own", pstr(p.getn("hostPort"))));
                }
            }
        }
        if !ports.is_empty() {
            y.push((
                s("expose"),
                Y::Seq(ports.iter().map(|p| s(&p.to_string())).collect()),
            ));
        }
        // readiness
        if ctr.role != "init" && inst.job.is_none() {
            self.readiness(&c, &wh, &ports, &mut y, &mut comments);
        }
        // restart policy
        if inst.job.is_none() && ctr.role != "init" {
            let rp = pstr(inst.spec.getn("restartPolicy"));
            let r = match rp.as_str() {
                "" | "Always" => "always",
                "OnFailure" => "on-failure",
                "Never" => "no",
                _ => "always",
            };
            y.push((s("restart"), s(r)));
            comments.push(format!(
                "restartPolicy {}: restart: {r}",
                if rp.is_empty() {
                    "Always (the default)"
                } else {
                    &rp
                }
            ));
        }
        // resources
        let res = c.getn("resources");
        let mem = res
            .getn("limits")
            .get("memory")
            .or_else(|| res.getn("requests").get("memory"))
            .cloned();
        if let Some(m) = mem.as_ref().and_then(quantity_mib_any) {
            y.push((s("mem_limit"), s(&format!("{m}m"))));
        }
        if let Some(cpu) = res.getn("limits").get("cpu").filter(|v| v.truthy()) {
            comments.push(format!("resources.limits.cpu {}: not modelled; a CPU limit's effect is a pause of the workload (P=workload, hydo guide faults)", pstr(cpu)));
        }
        if let Some(l) = c.get("lifecycle").filter(|v| v.truthy()) {
            let ks: Vec<String> = l.iter_keys().iter().map(pstr).collect();
            self.warn("lifecycle", format!("{wh}: lifecycle {}: not run; add the commands to the service's setup: (postStart) if the program needs them", ks.join(", ")));
        }
        if c.getn("livenessProbe").truthy() {
            comments.push("livenessProbe: not modelled; hydo does not restart a service whose probe fails (restart: covers a process that ends)".into());
        }
        // volumes
        let (mounts, named, perms) = self.volumes(&inst, &c, &ctr, &wh, &mut texts)?;
        if !mounts.is_empty() {
            y.push((s("volumes"), Y::Seq(mounts)));
        }
        // the one-shot jobs
        if ctr.role == "init" {
            y.push((s("x-hydo-job"), s("init")));
        } else if let Some(j) = &inst.job {
            y.push((s("x-hydo-job"), s(j)));
            if let Some(t) = &inst.schedule {
                comments.push(format!("CronJob timetable \"{t}\": hydo does not run it on a timer; run its command from the workload where the test needs it"));
                self.note(format!("CronJob {}: its timetable \"{t}\" does not run in hydo; the service {} keeps the image for `hydo-oci`", inst.workload, svc_name(&ctr.name)));
            }
        }
        if ctr.role == "app" || ctr.role == "sidecar" {
            self.pod_warnings(&inst);
        }
        if !comments.is_empty() {
            y.push((s("x-hydo-comments"), strs(&comments)));
        }
        Ok((Y::Map(y), named, perms, texts))
    }

    fn missing(&mut self, wh: &str, kind: &str, name: &str, optional: bool) {
        if optional {
            self.note(format!(
                "{wh}: {kind} {name} is not in the manifests (optional); left out"
            ));
        } else {
            self.warn("missing", format!("{wh}: {kind} {name} is not in the manifests; left out (add it to the files you import, or render the chart with the values that create it)"));
        }
    }

    /// The address the Compose importer gives the k-th service.
    fn addr(&self, k: usize) -> String {
        if self.cv.no_addr {
            "127.0.0.1".into()
        } else if self.cv.bridge {
            format!("10.89.0.{}", 11 + k)
        } else {
            format!("127.0.0.{}", 11 + k)
        }
    }

    fn readiness(
        &mut self,
        c: &Y,
        wh: &str,
        ports: &[i128],
        y: &mut Vec<(Y, Y)>,
        comments: &mut Vec<String>,
    ) {
        let (probe, which) = if c.getn("readinessProbe").truthy() {
            (c.getn("readinessProbe").clone(), "readinessProbe")
        } else if c.getn("startupProbe").truthy() {
            (c.getn("startupProbe").clone(), "startupProbe")
        } else {
            (Y::Null, "")
        };
        let num = |k: &str, d: i128| probe.get(k).and_then(int_of).unwrap_or(d);
        let budget =
            num("initialDelaySeconds", 0) + num("periodSeconds", 10) * num("failureThreshold", 3);
        let timeout = 120.max(600.min(budget + 60));
        // at least 5 s: a check in the guest starts a process in the image's root (hydo-oci),
        // which takes longer than Kubernetes' default of 1 s on a container runtime
        let tmo = num("timeoutSeconds", 1).clamp(5, 30);
        let set = |y: &mut Vec<(Y, Y)>, r: String, from: String| {
            y.push((s("x-hydo-ready"), s(&r)));
            y.push((s("x-hydo-ready-timeout"), Y::Int(timeout)));
            y.push((s("x-hydo-ready-from"), s(&from)));
        };
        if probe.is_map() {
            if let Some(h) = probe.get("httpGet").filter(|v| v.is_map()) {
                let port = Self::port_of(c, h.getn("port"));
                let path = {
                    let p = pstr(h.getn("path"));
                    if h.getn("path").is_none() || p.is_empty() {
                        "/".to_string()
                    } else if p.starts_with('/') {
                        p
                    } else {
                        format!("/{p}")
                    }
                };
                let https = pstr(h.getn("scheme")).eq_ignore_ascii_case("https");
                if !as_list(h.get("httpHeaders")).is_empty() {
                    comments.push(format!(
                        "{which}.httpGet.httpHeaders: not sent by the check"
                    ));
                }
                match port {
                    Some(p) if !https => {
                        let url = format!("http://{{addr}}:{p}{path}");
                        set(
                            y,
                            format!(
                                "wget -q -T {tmo} -O /dev/null {}",
                                crate::shlex::quote(&url)
                            ),
                            format!("{which} httpGet {path} on port {p} (a wget from the guest)"),
                        );
                        return;
                    }
                    Some(p) => {
                        set(y, format!("tcp:{p}"), format!("{which} httpGet over HTTPS on port {p}: a TCP connect (the guest's wget speaks no TLS)"));
                        return;
                    }
                    None => self.warn(
                        "probe",
                        format!(
                            "{wh}: {which}.httpGet port {}: not a port of the container",
                            pstr(h.getn("port"))
                        ),
                    ),
                }
            } else if let Some(t) = probe.get("tcpSocket").filter(|v| v.is_map()) {
                match Self::port_of(c, t.getn("port")) {
                    Some(p) => {
                        set(
                            y,
                            format!("tcp:{p}"),
                            format!("{which} tcpSocket on port {p}"),
                        );
                        return;
                    }
                    None => self.warn(
                        "probe",
                        format!(
                            "{wh}: {which}.tcpSocket port {}: not a port of the container",
                            pstr(t.getn("port"))
                        ),
                    ),
                }
            } else if let Some(g) = probe.get("grpc").filter(|v| v.is_map()) {
                if let Some(p) = int_of(g.getn("port")) {
                    set(y, format!("tcp:{p}"), format!("{which} grpc on port {p}: a TCP connect (no gRPC health client in the guest)"));
                    return;
                }
            } else if let Some(e) = probe.get("exec").filter(|v| v.is_map()) {
                let cmd: Vec<String> = as_list(e.get("command"))
                    .iter()
                    .map(|x| esc(&pstr(x)))
                    .collect();
                if !cmd.is_empty() {
                    let mut test = vec!["CMD".to_string()];
                    test.extend(cmd);
                    y.push((
                        s("healthcheck"),
                        map(vec![
                            ("test", strs(&test)),
                            ("timeout", s(&format!("{tmo}s"))),
                            ("interval", s(&format!("{}s", num("periodSeconds", 10)))),
                            ("retries", Y::Int(num("failureThreshold", 3))),
                            (
                                "start_period",
                                s(&format!("{}s", num("initialDelaySeconds", 0))),
                            ),
                        ]),
                    ));
                    y.push((
                        s("x-hydo-ready-from"),
                        s(&format!("the {which}'s exec command, run inside the image")),
                    ));
                    return;
                }
            }
        }
        match ports.first() {
            Some(p) => set(y, format!("tcp:{p}"), "no probe; a TCP connect to its first port (TODO: a real readiness check if the port opens before the service can serve)".into()),
            None => {
                set(y, "true".into(), "TODO no probe and no port: `true` means started".into());
                self.warn("probe", format!("{wh}: no readiness probe and no port: readiness is just `started`; write a ready: check"));
            }
        }
    }

    /// Pod-level settings that do not map, once per workload.
    fn pod_warnings(&mut self, inst: &Inst) {
        let wh = format!("{} {}", inst.kind, inst.workload);
        let sp = &inst.spec;
        let present: Vec<&str> = SCHEDULING
            .iter()
            .copied()
            .filter(|k| sp.get(k).is_some_and(has_content))
            .collect();
        if !present.is_empty() {
            self.warn("scheduling", format!("{wh}: {}: scheduling does not apply, hydo runs every pod on one machine; ignored", present.join(", ")));
        }
        for (k, why) in [
            ("hostNetwork", "every service has its own network namespace on one bridge (or, with --network loopback, all share one loopback network); a peer named localhost:<port> must become its name"),
            ("hostPID", "pid namespaces are not modelled; services see each other's processes"),
            ("hostIPC", "ipc namespaces are not modelled"),
            ("shareProcessNamespace", "services see each other's processes anyway"),
        ] {
            if sp.getn(k).truthy() {
                self.warn(k, format!("{wh}: {k}: {why}"));
            }
        }
        if as_list(sp.getn("securityContext").get("sysctls"))
            .iter()
            .any(Y::truthy)
        {
            self.warn("sysctls", format!("{wh}: securityContext.sysctls: not set; add sysctl -w to the service's setup: if the program needs them"));
        }
        if sp.getn("dnsConfig").truthy()
            || matches!(pstr(sp.getn("dnsPolicy")).as_str(), "None" | "Default")
        {
            self.note(format!("{wh}: dnsPolicy/dnsConfig: there is no DNS in the guest; names resolve through /etc/hosts"));
        }
        if !as_list(sp.get("hostAliases")).is_empty() {
            self.warn("hostAliases", format!("{wh}: hostAliases: not added to /etc/hosts; add them to root.text's /etc/hosts"));
        }
    }

    /// The container's volume mounts as Compose volumes; the named volumes; and the data-disk
    /// volumes with what they are, for the setup.
    #[allow(clippy::type_complexity)]
    fn volumes(
        &mut self,
        inst: &Inst,
        c: &Y,
        ctr: &Ctr,
        wh: &str,
        texts: &mut Vec<String>,
    ) -> R<(Vec<Y>, Vec<String>, Vec<(String, String)>)> {
        let mut out = Vec::new();
        let mut named = Vec::new();
        let mut perms: Vec<(String, String)> = Vec::new();
        let pvols = as_list(inst.spec.get("volumes"));
        // emptyDir volumes that several containers of the pod use are shared on the data disk
        let users = |vname: &str| -> usize {
            let mut n = 0;
            for key in ["initContainers", "containers"] {
                for cc in as_list(inst.spec.get(key)) {
                    if as_list(cc.get("volumeMounts"))
                        .iter()
                        .any(|m| pstr(m.getn("name")) == vname)
                    {
                        n += 1;
                    }
                }
            }
            n
        };
        let base = if self.multi_ns {
            format!("{}-{}", inst.ns, inst.pod)
        } else {
            inst.pod.clone()
        };
        for m in as_list(c.get("volumeMounts")) {
            let vname = pstr(m.getn("name"));
            let target = pstr(m.getn("mountPath"));
            if target.trim_end_matches('/') == "/tmp" {
                self.note(format!("{wh}: volume {vname} on /tmp is not mounted: an image's /tmp is the guest's, shared by every service"));
                continue;
            }
            let ro = m.getn("readOnly").truthy();
            let sub = if m.getn("subPathExpr").truthy() {
                expand(&pstr(m.getn("subPathExpr")), &[]).0
            } else if m.getn("subPath").truthy() {
                pstr(m.getn("subPath"))
            } else {
                String::new()
            };
            if !sub.is_empty() && !safe_rel(&sub) {
                self.warn(
                    "volumes",
                    format!(
                        "{wh}: volume {vname} subPath {sub}: not a plain relative path; skipped"
                    ),
                );
                continue;
            }
            let volume = |src: String, copy: bool| -> Y {
                let mut v = vec![
                    ("type", s("volume")),
                    ("source", s(&src)),
                    ("target", s(&target)),
                ];
                if ro {
                    v.push(("read_only", Y::Bool(true)));
                }
                if !copy {
                    v.push(("x-hydo-copy-up", Y::Bool(false)));
                }
                map(v)
            };
            let with_sub = |n: String| {
                if sub.is_empty() {
                    n
                } else {
                    format!("{n}-{}", sub.replace('/', "-"))
                }
            };
            // a claim of the StatefulSet's templates: <template>-<pod>, as Kubernetes names it
            if inst.claims.contains(&vname) && !pvols.iter().any(|v| pstr(v.getn("name")) == vname)
            {
                let n = with_sub(format!("{vname}-{base}"));
                named.push(n.clone());
                perms.push((safe_name(&n), format!("claim {target}")));
                out.push(volume(n, false));
                continue;
            }
            let Some(v) = pvols
                .iter()
                .find(|v| pstr(v.getn("name")) == vname)
                .cloned()
            else {
                self.warn(
                    "volumes",
                    format!("{wh}: volumeMount {vname} names no volume of the pod; skipped"),
                );
                continue;
            };
            if let Some(pvc) = v.get("persistentVolumeClaim").filter(|x| x.is_map()) {
                let n = with_sub(pstr(pvc.getn("claimName")));
                named.push(n.clone());
                perms.push((safe_name(&n), format!("claim {target}")));
                out.push(volume(n, false));
            } else if let Some(ed) = v.get("emptyDir") {
                let mem = pstr(ed.getn("medium")) == "Memory";
                if mem && users(&vname) < 2 {
                    let mut t = vec![("type", s("tmpfs")), ("target", s(&target))];
                    if let Some(mib) = ed.get("sizeLimit").and_then(quantity_mib_any) {
                        t.push(("tmpfs", map(vec![("size", s(&format!("{mib}m")))])));
                    }
                    out.push(map(t));
                } else {
                    let n = with_sub(format!("{base}-{vname}"));
                    named.push(n.clone());
                    perms.push((safe_name(&n), format!("emptyDir {target}")));
                    out.push(volume(n, false));
                    if mem {
                        self.note(format!("{wh}: emptyDir {vname} (medium: Memory) is shared by several containers of the pod, so it is a directory on the data disk, not a tmpfs"));
                    }
                }
            } else if let Some(cm) = v.get("configMap").filter(|x| x.is_map()) {
                let src = vec![("configMap", cm.clone())];
                if let Some(p) = self.files_volume(
                    inst,
                    &src,
                    v.getn("configMap").getn("defaultMode"),
                    &sub,
                    wh,
                    texts,
                ) {
                    out.push(bind(&p, &target));
                }
            } else if let Some(sc) = v.get("secret").filter(|x| x.is_map()) {
                let src = vec![("secret", sc.clone())];
                if let Some(p) = self.files_volume(
                    inst,
                    &src,
                    v.getn("secret").getn("defaultMode"),
                    &sub,
                    wh,
                    texts,
                ) {
                    out.push(bind(&p, &target));
                }
            } else if let Some(pj) = v.get("projected").filter(|x| x.is_map()) {
                let mut src = Vec::new();
                for x in as_list(pj.get("sources")) {
                    if let Some(cm) = x.get("configMap") {
                        src.push(("configMap", cm.clone()));
                    } else if let Some(sc) = x.get("secret") {
                        src.push(("secret", sc.clone()));
                    } else {
                        let k: Vec<String> = x.iter_keys().iter().map(pstr).collect();
                        self.note(format!("{wh}: projected volume {vname}: source {} is left out (there is no API server in hydo)", k.join(", ")));
                    }
                }
                if !src.is_empty() {
                    if let Some(p) =
                        self.files_volume(inst, &src, pj.getn("defaultMode"), &sub, wh, texts)
                    {
                        out.push(bind(&p, &target));
                    }
                }
            } else if let Some(hp) = v.get("hostPath").filter(|x| x.is_map()) {
                let p = pstr(hp.getn("path"));
                let n = with_sub(format!(
                    "hostpath-{}",
                    p.trim_matches('/').replace('/', "-")
                ));
                self.warn("volumes", format!("{wh}: hostPath {p}: the guest is a separate machine; an empty directory on the data disk (/data/vol/{}) instead", safe_name(&n)));
                named.push(n.clone());
                perms.push((safe_name(&n), format!("hostPath {target}")));
                out.push(volume(n, false));
            } else if v.get("downwardAPI").is_some() {
                self.warn("volumes", format!("{wh}: downwardAPI volume {vname}: not written; skipped (an env var with fieldRef is imported)"));
            } else {
                let k: Vec<String> = v
                    .iter_keys()
                    .iter()
                    .map(pstr)
                    .filter(|k| k != "name")
                    .collect();
                let n = with_sub(format!("{base}-{vname}"));
                self.warn("volumes", format!("{wh}: volume {vname} ({}): a storage hydo does not have; an empty directory on the data disk (/data/vol/{}) instead", k.join(", "), safe_name(&n)));
                named.push(n.clone());
                perms.push((safe_name(&n), format!("claim {target}")));
                out.push(volume(n, false));
            }
        }
        let _ = ctr;
        Ok((out, named, perms))
    }

    /// The files of a configMap, secret or projected volume, written under k8s-import/; the path
    /// to bind (the folder, or one file of it for a subPath).
    fn files_volume(
        &mut self,
        inst: &Inst,
        sources: &[(&str, Y)],
        default_mode: &Y,
        sub: &str,
        wh: &str,
        texts: &mut Vec<String>,
    ) -> Option<PathBuf> {
        let dmode = int_of(default_mode).map(|m| m as u32).unwrap_or(0o644);
        let secret = sources.iter().any(|(k, _)| *k == "secret");
        let mut files: Vec<(String, Vec<u8>, u32)> = Vec::new();
        let mut key = String::new();
        for (kind, src) in sources {
            let name = if *kind == "secret" {
                pstr(src.getn("secretName"))
            } else {
                pstr(src.getn("name"))
            };
            let name = if name.is_empty() && *kind == "secret" {
                pstr(src.getn("name"))
            } else {
                name
            };
            key.push_str(&format!("{kind}/{name};"));
            let data = if *kind == "secret" {
                self.secret(&inst.ns, &name)
            } else {
                self.configmap(&inst.ns, &name)
            };
            let Some(data) = data else {
                self.missing(
                    wh,
                    if *kind == "secret" {
                        "Secret"
                    } else {
                        "ConfigMap"
                    },
                    &name,
                    src.getn("optional").truthy(),
                );
                continue;
            };
            let items = as_list(src.get("items"));
            if items.is_empty() {
                for (k, v) in data {
                    if *kind == "configMap" {
                        texts.push(String::from_utf8_lossy(&v).into_owned());
                    }
                    files.push((k, v, dmode));
                }
            } else {
                for it in items {
                    let (k, p) = (pstr(it.getn("key")), pstr(it.getn("path")));
                    let mode = int_of(it.getn("mode")).map(|m| m as u32).unwrap_or(dmode);
                    key.push_str(&format!("{k}>{p}:{mode};"));
                    match data.iter().find(|(x, _)| *x == k) {
                        Some((_, v)) if safe_rel(&p) => {
                            if *kind == "configMap" {
                                texts.push(String::from_utf8_lossy(v).into_owned());
                            }
                            files.push((p, v.clone(), mode));
                        }
                        Some(_) => self.warn(
                            "volumes",
                            format!(
                                "{wh}: item path {p} of {name}: not a plain relative path; skipped"
                            ),
                        ),
                        None => self.missing(
                            wh,
                            if *kind == "secret" {
                                "Secret"
                            } else {
                                "ConfigMap"
                            },
                            &format!("{name} (key {k})"),
                            it.getn("optional").truthy() || src.getn("optional").truthy(),
                        ),
                    }
                }
            }
        }
        if files.is_empty() {
            return None;
        }
        key.push_str(&format!("mode {dmode}"));
        let simple =
            sources.len() == 1 && as_list(sources[0].1.get("items")).is_empty() && dmode == 0o644;
        let first = &sources[0];
        let fname = if first.0 == "secret" {
            pstr(first.1.getn("secretName"))
        } else {
            pstr(first.1.getn("name"))
        };
        let dname = if simple {
            safe_name(&fname)
        } else {
            format!("{}-{}", safe_name(&fname), short_hash(&key))
        };
        let dir = self
            .cv
            .dir
            .join(if secret { "secrets" } else { "configmap" })
            .join(if self.multi_ns {
                format!("{}-{dname}", inst.ns)
            } else {
                dname
            });
        for (p, v, mode) in files {
            self.add_file(dir.join(&p), v, mode);
        }
        if sub.is_empty() {
            Some(dir)
        } else {
            let p = dir.join(sub);
            if self.written.contains(&p) {
                Some(p)
            } else {
                self.warn(
                    "volumes",
                    format!("{wh}: subPath {sub} is not a key of the volume; skipped"),
                );
                None
            }
        }
    }
}

fn bind(p: &Path, target: &str) -> Y {
    map(vec![
        ("type", s("bind")),
        ("source", s(&p.display().to_string())),
        ("target", s(target)),
        ("read_only", Y::Bool(true)),
    ])
}

/// A value with something in it (an affinity of empty mappings has none).
fn has_content(v: &Y) -> bool {
    match v {
        Y::Map(m) => m.iter().any(|(_, x)| has_content(x)),
        Y::Seq(l) => l.iter().any(has_content),
        Y::Null => false,
        Y::Str(t) => !t.is_empty(),
        _ => true,
    }
}

/// Helm's hook names of an object (`helm.sh/hook: pre-install,pre-upgrade`).
fn hooks(o: &Obj) -> Vec<String> {
    let a =
        o.y.getn("metadata")
            .getn("annotations")
            .getn("helm.sh/hook");
    if a.is_none() {
        return Vec::new();
    }
    pstr(a)
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// `from` waits (directly or through others) for `to`.
fn reaches(deps: &[Vec<(usize, String)>], from: usize, to: usize) -> bool {
    let mut stack = vec![from];
    let mut seen = BTreeSet::new();
    while let Some(x) = stack.pop() {
        if x == to {
            return true;
        }
        if !seen.insert(x) {
            continue;
        }
        stack.extend(deps[x].iter().map(|(d, _)| *d));
    }
    false
}

// ---------------------------------------------------------------- the command

/// What `hydo k8s` was asked.
#[derive(Debug, Default)]
struct Args {
    inputs: Vec<String>,
    helm: Option<String>,
    values: Vec<String>,
    sets: Vec<String>,
    release: Option<String>,
    namespace: Option<String>,
    kustomize: Option<String>,
    out: Option<String>,
    o: Opts,
}

fn parse(a: &[String]) -> Result<Args, String> {
    let mut r = Args::default();
    let mut i = 0;
    while i < a.len() {
        let x = a[i].as_str();
        let with_value = [
            "--out",
            "-o",
            "--name",
            "--images-dir",
            "--lock",
            "--network",
            "--helm",
            "--values",
            "-f",
            "--set",
            "--release",
            "--namespace",
            "-n",
            "--kustomize",
        ];
        if with_value.contains(&x) {
            let v = a.get(i + 1).cloned().ok_or(format!("{x} needs a value"))?;
            match x {
                "--out" | "-o" => r.out = Some(v),
                "--name" => r.o.name = Some(v),
                "--images-dir" => r.o.images_dir = Some(v),
                "--lock" => r.o.lock = Some(v),
                "--network" => r.o.network = Some(v),
                "--helm" => r.helm = Some(v),
                "--values" | "-f" => r.values.push(v),
                "--set" => r.sets.push(v),
                "--release" => r.release = Some(v),
                "--namespace" | "-n" => r.namespace = Some(v),
                _ => r.kustomize = Some(v),
            }
            i += 2;
            continue;
        }
        match x {
            "--no-resolve" => r.o.no_resolve = true,
            "--no-addr" => r.o.no_addr = true,
            _ if x.starts_with('-') && x != "-" => {
                return Err(format!("unknown option {x} (hydo k8s --help)"))
            }
            _ => r.inputs.push(x.to_string()),
        }
        i += 1;
    }
    if r.inputs.is_empty() && r.helm.is_none() && r.kustomize.is_none() {
        return Err("name the manifests: files, a folder, --helm CHART or --kustomize DIR (hydo k8s --help)".into());
    }
    if !matches!(
        r.o.network.as_deref().unwrap_or("bridge"),
        "bridge" | "loopback"
    ) {
        return Err("--network bridge or loopback".into());
    }
    Ok(r)
}

/// The rendered or read manifests: (label, text), what they are for the header, and whether a
/// renderer made them (then they are saved in k8s-import/manifests.yaml).
struct Inputs {
    texts: Vec<(String, String)>,
    from: String,
    title: String,
    rendered: bool,
}

fn helm_argv(a: &Args, chart: &str) -> Vec<String> {
    let mut v = vec![
        "helm".to_string(),
        "template".into(),
        a.release.clone().unwrap_or_else(|| "hydo".into()),
        chart.into(),
    ];
    if let Some(ns) = &a.namespace {
        v.push("--namespace".into());
        v.push(ns.clone());
    }
    for f in &a.values {
        v.push("--values".into());
        v.push(f.clone());
    }
    for x in &a.sets {
        v.push("--set".into());
        v.push(x.clone());
    }
    v
}

fn read_inputs(a: &Args, odir: &str) -> R<Inputs> {
    let mut texts = Vec::new();
    let mut from: Vec<String> = Vec::new();
    let mut title = String::new();
    let mut rendered = false;
    let rel = |p: &str| crate::relpath(&hydo_manifest::py::abspath(p), odir);
    let helm = |chart: &str,
                texts: &mut Vec<(String, String)>,
                from: &mut Vec<String>,
                title: &mut String|
     -> R<()> {
        let argv = helm_argv(a, chart);
        let cmd = shown_command(&argv);
        let t = render(
            &argv,
            &format!("the Helm chart {chart}"),
            &format!("{cmd} > rendered.yaml; hydo k8s rendered.yaml"),
        )?;
        texts.push((format!("helm template {chart}"), t));
        let shown = if Path::new(chart).exists() {
            rel(chart)
        } else {
            chart.to_string()
        };
        from.push(format!(
            "the Helm chart {shown}, rendered with:\n  {}",
            cmd.replacen(chart, &shown, 1)
        ));
        if title.is_empty() {
            *title = format!(
                "the Helm chart {}",
                Path::new(chart)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(chart)
            );
        }
        Ok(())
    };
    let kustomize = |dir: &str,
                     texts: &mut Vec<(String, String)>,
                     from: &mut Vec<String>,
                     title: &mut String|
     -> R<()> {
        let argv = vec!["kubectl".to_string(), "kustomize".into(), dir.into()];
        let how = format!("kubectl kustomize {dir} > rendered.yaml; hydo k8s rendered.yaml");
        let t = match render(&argv, &format!("the Kustomize folder {dir}"), &how) {
            Ok(t) => t,
            Err(e) => {
                let alt = vec!["kustomize".to_string(), "build".into(), dir.into()];
                render(&alt, &format!("the Kustomize folder {dir}"), &how).map_err(|_| e)?
            }
        };
        texts.push((format!("kustomize {dir}"), t));
        from.push(format!(
            "the Kustomize folder {}, rendered with kubectl kustomize",
            rel(dir)
        ));
        if title.is_empty() {
            *title = format!("the Kustomize folder {}", rel(dir));
        }
        Ok(())
    };
    if let Some(c) = &a.helm {
        helm(c, &mut texts, &mut from, &mut title)?;
        rendered = true;
    }
    if let Some(d) = &a.kustomize {
        kustomize(d, &mut texts, &mut from, &mut title)?;
        rendered = true;
    }
    let mut files: Vec<String> = Vec::new();
    for p in &a.inputs {
        if p == "-" {
            let mut t = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)
                .map_err(|e| ComposeError(format!("stdin: {e}")))?;
            texts.push(("stdin".into(), t));
            from.push("the manifests on standard input".into());
            continue;
        }
        let path = Path::new(p);
        if path.is_dir() {
            match kind_of_folder(path) {
                "helm" => {
                    helm(p, &mut texts, &mut from, &mut title)?;
                    rendered = true;
                }
                "kustomize" => {
                    kustomize(p, &mut texts, &mut from, &mut title)?;
                    rendered = true;
                }
                _ => {
                    let mut fs = Vec::new();
                    manifest_files(path, &mut fs).map_err(|e| ComposeError::os(e, p))?;
                    if fs.is_empty() {
                        return err(format!(
                            "{p}: no manifest files (.yaml, .yml, .json) in the folder"
                        ));
                    }
                    if title.is_empty() {
                        title = format!("the manifests in {}", rel(p));
                    }
                    from.push(format!("the manifests in {}", rel(p)));
                    for f in fs {
                        let fsx = f.display().to_string();
                        texts.push((rel(&fsx), crate::interp::read_text(&fsx)?));
                    }
                }
            }
        } else {
            texts.push((rel(p), crate::interp::read_text(p)?));
            files.push(rel(p));
        }
    }
    if !files.is_empty() {
        if title.is_empty() {
            title = files.join(", ");
        }
        from.push(files.join(", "));
    }
    Ok(Inputs {
        texts,
        from: from.join("\nand "),
        title,
        rendered,
    })
}

/// The rendered manifests without the values of the Secrets, for k8s-import/manifests.yaml.
fn redacted(texts: &[(String, String)]) -> String {
    let mut out = String::from("# What hydo k8s imported, as the renderer printed it. The values of every Secret are\n# left out; they are in k8s-import/secrets/, which version control ignores.\n");
    for (label, t) in texts {
        out.push_str(&format!("# ---- {label}\n"));
        let mut part = String::new();
        let flush = |part: &mut String, out: &mut String| {
            let is_secret = part.lines().any(|l| l.trim_end() == "kind: Secret");
            if is_secret {
                // the Secret without its data: and stringData: sections
                out.push_str("---\n");
                let mut skip = false;
                for l in part.lines() {
                    let top = !l.starts_with([' ', '\t', '-', '#']) && !l.trim().is_empty();
                    if top {
                        skip = l.starts_with("data:") || l.starts_with("stringData:");
                        if skip {
                            let k = l.split(':').next().unwrap_or("data");
                            out.push_str(&format!(
                                "{k}: {{}}   # the values are left out (k8s-import/secrets/)\n"
                            ));
                            continue;
                        }
                    }
                    if !skip {
                        out.push_str(l);
                        out.push('\n');
                    }
                }
            } else if !part.trim().is_empty() {
                out.push_str("---\n");
                out.push_str(part);
                if !part.ends_with('\n') {
                    out.push('\n');
                }
            }
            part.clear();
        };
        for ln in t.split_inclusive('\n') {
            if ln.trim_end() == "---" {
                flush(&mut part, &mut out);
            } else {
                part.push_str(ln);
            }
        }
        flush(&mut part, &mut out);
    }
    out
}

/// `hydo k8s ...`: the exit status.
pub fn main(argv: &[String], clock_start: Option<i64>) -> i32 {
    let mut reg = registry::Http::new();
    main_with(argv, &mut reg, clock_start)
}

/// [`main`] with a given registry (the tests replay recorded registry answers).
pub fn main_with(a: &[String], reg: &mut dyn registry::Registry, clock_start: Option<i64>) -> i32 {
    if a.is_empty() || a[0] == "-h" || a[0] == "--help" {
        println!("{USAGE}");
        return if a.is_empty() { 2 } else { 0 };
    }
    let mut args = match parse(a) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{TOOL}: {e}");
            return 2;
        }
    };
    args.o.clock_start = clock_start;
    let out = match args.out.clone().filter(|x| !x.is_empty()) {
        Some(x) => x,
        None => "hydo.yaml".into(),
    };
    let oabs = hydo_manifest::py::abspath(&out);
    let odir = hydo_manifest::py::dirname(&oabs);
    let inputs = match read_inputs(&args, &odir) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{TOOL}: {e}");
            return 1;
        }
    };
    let kdir = Path::new(&odir).join("k8s-import");
    let cv = Conv {
        dir: kdir.clone(),
        namespace: args.namespace.clone().unwrap_or_else(|| "default".into()),
        bridge: args.o.network.as_deref().unwrap_or("bridge") == "bridge" && !args.o.no_addr,
        no_addr: args.o.no_addr,
    };
    let conv = match convert(&inputs.texts, &cv) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{TOOL}: {e}");
            return 1;
        }
    };
    if let Err(e) = write_files(
        &kdir,
        &conv,
        inputs.rendered.then(|| redacted(&inputs.texts)),
    ) {
        eprintln!("{TOOL}: {e}");
        return 1;
    }
    let mut o = args.o.clone();
    if o.name.is_none() {
        o.name = Some(match (&args.release, &args.helm) {
            (Some(r), _) => r.clone(),
            _ => {
                let t = inputs.title.rsplit(' ').next().unwrap_or("k8s").to_string();
                Path::new(&t)
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .unwrap_or("k8s")
                    .to_string()
            }
        });
    }
    if o.lock.as_deref().unwrap_or("").is_empty() {
        o.lock = Some(
            Path::new(&odir)
                .join("hydo-k8s.lock.json")
                .display()
                .to_string(),
        );
    }
    o.origin = Some(Origin {
        doc: conv.doc.clone(),
        tool: TOOL.into(),
        from: inputs.from.clone(),
        title: inputs.title.clone(),
        reference:
            "docs/design/k8s-import.md (`hydo guide workflow`, \"From Kubernetes manifests\")"
                .into(),
        warnings: conv.warnings.clone(),
        notes: conv.notes.clone(),
    });
    let vpath = kdir.join("manifests.yaml").display().to_string();
    let mut imp = Importer::new(&vpath, &out, o, reg);
    let text = match imp.run() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{TOOL}: {e}");
            return 1;
        }
    };
    crate::write_outputs(&mut imp, &text, &out, TOOL)
}

/// Write k8s-import/: the ConfigMaps' and Secrets' files, the git-ignore of secrets/, and the
/// rendered manifests. The folders configmap/ and secrets/ are the import's own: made again.
pub fn write_files(
    kdir: &Path,
    conv: &Converted,
    manifests: Option<String>,
) -> Result<(), ComposeError> {
    use std::os::unix::fs::PermissionsExt;
    for sub in ["configmap", "secrets"] {
        let d = kdir.join(sub);
        if d.is_dir() {
            std::fs::remove_dir_all(&d)
                .map_err(|e| ComposeError::os(e, &d.display().to_string()))?;
        }
    }
    let mk = |d: &Path| {
        std::fs::create_dir_all(d).map_err(|e| ComposeError::os(e, &d.display().to_string()))
    };
    mk(kdir)?;
    if conv
        .files
        .iter()
        .any(|f| f.path.starts_with(kdir.join("secrets")))
    {
        mk(&kdir.join("secrets"))?;
        let g = kdir.join("secrets/.gitignore");
        std::fs::write(&g, "# written by hydo k8s: the values of Kubernetes Secrets never go into version control\n*\n")
            .map_err(|e| ComposeError::os(e, &g.display().to_string()))?;
    }
    for f in &conv.files {
        if let Some(p) = f.path.parent() {
            mk(p)?;
        }
        std::fs::write(&f.path, &f.bytes)
            .map_err(|e| ComposeError::os(e, &f.path.display().to_string()))?;
        std::fs::set_permissions(&f.path, std::fs::Permissions::from_mode(f.mode & 0o7777))
            .map_err(|e| ComposeError::os(e, &f.path.display().to_string()))?;
    }
    let m = kdir.join("manifests.yaml");
    match manifests {
        Some(t) => {
            std::fs::write(&m, t).map_err(|e| ComposeError::os(e, &m.display().to_string()))?
        }
        None => {
            if m.is_file() {
                let _ = std::fs::remove_file(&m);
            }
        }
    }
    Ok(())
}

/// Whether a folder holds Kubernetes manifests (a workload kind in a .yaml file), for `hydo
/// onboard`: the first such file.
pub fn has_workloads(dir: &Path) -> Option<PathBuf> {
    let mut fs = Vec::new();
    manifest_files(dir, &mut fs).ok()?;
    fs.into_iter().take(200).find(|f| {
        std::fs::read_to_string(f).is_ok_and(|t| {
            t.lines().any(|l| {
                l.strip_prefix("kind:")
                    .is_some_and(|k| WORKLOADS.contains(&k.trim()))
            }) && t.contains("apiVersion:")
        })
    })
}

#[cfg(test)]
mod tests;
