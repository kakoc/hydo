//! One test per mapping of docs/design/k8s-import.md: each converts a small manifest and looks at
//! the Compose-shaped document (and the files) it gives.

use super::*;
use crate::pyval::repr;

fn cv() -> Conv {
    Conv {
        dir: PathBuf::from("/out/k8s-import"),
        namespace: "default".into(),
        bridge: true,
        no_addr: false,
    }
}

fn conv(y: &str) -> Converted {
    convert(&[("m.yaml".to_string(), y.to_string())], &cv()).unwrap()
}

fn svc<'a>(c: &'a Converted, n: &str) -> &'a Y {
    c.doc
        .getn("services")
        .get(n)
        .unwrap_or_else(|| panic!("no service {n} in {}", repr(&c.doc)))
}

fn names(c: &Converted) -> Vec<String> {
    c.doc
        .getn("services")
        .iter_keys()
        .iter()
        .map(pstr)
        .collect()
}

fn list(y: &Y) -> Vec<String> {
    as_list(Some(y)).iter().map(pstr).collect()
}

fn warned(c: &Converted, part: &str) -> bool {
    c.warnings.iter().any(|(_, t)| t.contains(part))
}

const DEPLOY: &str = "apiVersion: apps/v1
kind: Deployment
metadata: {name: web, labels: {app: web}}
spec:
  replicas: 3
  selector: {matchLabels: {app: web}}
  template:
    metadata: {labels: {app: web}}
    spec:
      containers:
        - name: web
          image: nginx:1.27
          command: [/bin/sh, -c]
          args: [\"echo $(GREETING) && exec nginx -g 'daemon off;'\"]
          workingDir: /srv
          env:
            - {name: GREETING, value: hello}
            - {name: COST, value: \"$5\"}
            - {name: POD, valueFrom: {fieldRef: {fieldPath: metadata.name}}}
            - {name: IP, valueFrom: {fieldRef: {fieldPath: status.podIP}}}
          ports: [{name: http, containerPort: 8080}, {containerPort: 9090}]
          readinessProbe: {httpGet: {path: /healthz, port: http}, periodSeconds: 5, failureThreshold: 4}
          resources: {limits: {memory: 300Mi, cpu: 500m}}
---
apiVersion: v1
kind: Service
metadata: {name: web}
spec:
  selector: {app: web}
  ports: [{port: 80, targetPort: http}]
";

#[test]
fn a_deployment_is_one_pod_with_its_command_env_and_probe() {
    let c = conv(DEPLOY);
    assert_eq!(names(&c), ["web"]);
    let s = svc(&c, "web");
    assert_eq!(pstr(s.getn("image")), "nginx:1.27");
    assert_eq!(list(s.getn("entrypoint")), ["/bin/sh", "-c"]);
    // $(GREETING) is filled in, and a $ is doubled for Compose's unescaping
    assert_eq!(
        list(s.getn("command")),
        ["echo hello && exec nginx -g 'daemon off;'"]
    );
    let env = s.getn("environment");
    assert_eq!(pstr(env.getn("COST")), "$$5");
    assert_eq!(pstr(env.getn("POD")), "web");
    assert_eq!(pstr(env.getn("IP")), "10.89.0.11");
    // what Kubernetes sets in every container: the pod's host name and the Services
    assert_eq!(pstr(env.getn("HOSTNAME")), "web");
    assert_eq!(pstr(env.getn("WEB_SERVICE_HOST")), "10.89.0.11");
    assert_eq!(pstr(env.getn("WEB_SERVICE_PORT")), "8080");
    assert_eq!(pstr(s.getn("working_dir")), "/srv");
    assert_eq!(list(s.getn("expose")), ["8080", "9090"]);
    assert_eq!(
        pstr(s.getn("x-hydo-ready")),
        "wget -q -T 5 -O /dev/null 'http://{addr}:8080/healthz'"
    );
    assert_eq!(int_of(s.getn("x-hydo-ready-timeout")), Some(120));
    assert_eq!(pstr(s.getn("restart")), "always");
    // a missing field is never Python's "None" (a missing dnsPolicy, restartPolicy, clusterIP)
    assert!(list(s.getn("x-hydo-comments"))
        .contains(&"restartPolicy Always (the default): restart: always".to_string()));
    assert!(!repr(&c.doc).contains("None") && !c.notes.iter().any(|n| n.contains("dnsPolicy")));
    assert_eq!(pstr(s.getn("mem_limit")), "300m");
    assert!(warned(&c, "replicas: 3: one pod is imported"));
    // the Service's names, and its port that differs from the target
    let al = list(s.getn("networks").getn("default").getn("aliases"));
    assert_eq!(
        al,
        [
            "web",
            "web.default",
            "web.default.svc",
            "web.default.svc.cluster.local"
        ]
    );
    assert!(warned(
        &c,
        "Service web: port 80 goes to the container's port 8080"
    ));
}

const STS: &str = "apiVersion: apps/v1
kind: StatefulSet
metadata: {name: db}
spec:
  replicas: 3
  serviceName: db-headless
  selector: {matchLabels: {app: db}}
  template:
    metadata: {labels: {app: db}}
    spec:
      securityContext: {fsGroup: 1001, runAsUser: 1001}
      affinity: {podAntiAffinity: {preferredDuringSchedulingIgnoredDuringExecution: [{weight: 1}]}}
      containers:
        - name: db
          image: example/db:2
          ports: [{name: peer, containerPort: 7000}]
          readinessProbe: {tcpSocket: {port: peer}}
          volumeMounts: [{name: data, mountPath: /var/lib/db}]
  volumeClaimTemplates:
    - metadata: {name: data}
      spec: {resources: {requests: {storage: 1Gi}}}
---
apiVersion: v1
kind: Service
metadata: {name: db-headless}
spec:
  clusterIP: None
  selector: {app: db}
  ports: [{port: 7000}]
";

#[test]
fn a_statefulset_is_one_service_per_replica_with_its_claims_and_a_group() {
    let c = conv(STS);
    assert_eq!(names(&c), ["db-0", "db-1", "db-2"]);
    let s1 = svc(&c, "db-1");
    assert_eq!(pstr(s1.getn("x-hydo-ready")), "tcp:7000");
    assert_eq!(pstr(s1.getn("user")), "1001");
    let v = &as_list(s1.get("volumes"))[0];
    assert_eq!(pstr(v.getn("source")), "data-db-1");
    assert_eq!(pstr(v.getn("target")), "/var/lib/db");
    assert_eq!(*v.getn("x-hydo-copy-up"), Y::Bool(false));
    let setup = pstr(s1.getn("x-hydo-setup"));
    assert!(
        setup.contains("chgrp -R 1001 /data/vol/data-db-1"),
        "{setup}"
    );
    assert!(setup.contains("chmod 0777 /data/vol/data-db-1"), "{setup}");
    // each pod's stable name under the headless Service
    let al = list(s1.getn("networks").getn("default").getn("aliases"));
    assert!(al.contains(&"db-1".to_string()), "{al:?}");
    assert!(
        al.contains(&"db-1.db-headless.default.svc.cluster.local".to_string()),
        "{al:?}"
    );
    // the Service name itself goes to the first pod
    let al0 = list(
        svc(&c, "db-0")
            .getn("networks")
            .getn("default")
            .getn("aliases"),
    );
    assert!(al0.contains(&"db-headless".to_string()), "{al0:?}");
    let g = &as_list(c.doc.get("x-hydo-groups"))[0];
    assert_eq!(pstr(g.getn("name")), "db");
    assert_eq!(list(g.getn("services")), ["db-0", "db-1", "db-2"]);
    assert!(warned(&c, "affinity: scheduling does not apply"));
}

#[test]
fn parallel_pods_start_together() {
    let c = conv(&STS.replace(
        "serviceName:",
        "podManagementPolicy: Parallel\n  serviceName:",
    ));
    assert!(svc(&c, "db-0").get("x-hydo-start-after").is_none());
    assert!(pstr(svc(&c, "db-2").getn("x-hydo-start-after")).contains("Parallel"));
}

const CONFIG: &str = "apiVersion: v1
kind: ConfigMap
metadata: {name: app-config}
data:
  app.conf: \"listen 8080\\n\"
  start.sh: \"#!/bin/sh\\nexec app\\n\"
  LOG_LEVEL: debug
---
apiVersion: v1
kind: Secret
metadata: {name: app-secret}
data:
  password: aHVudGVyMg==
stringData:
  token: abc
---
apiVersion: v1
kind: Pod
metadata: {name: app}
spec:
  initContainers:
    - name: migrate
      image: example/app:1
      command: [/app, migrate]
  containers:
    - name: app
      image: example/app:1
      envFrom: [{configMapRef: {name: app-config}, prefix: CFG_}]
      env:
        - {name: DB_PASSWORD, valueFrom: {secretKeyRef: {name: app-secret, key: password}}}
        - {name: DSN, value: \"postgres://app:$(DB_PASSWORD)@db:5432/app\"}
        - {name: MISSING, valueFrom: {configMapKeyRef: {name: nowhere, key: x}}}
      volumeMounts:
        - {name: config, mountPath: /etc/app}
        - {name: scripts, mountPath: /start.sh, subPath: start.sh}
        - {name: creds, mountPath: /run/creds}
        - {name: scratch, mountPath: /scratch}
        - {name: cache, mountPath: /cache}
  volumes:
    - {name: config, configMap: {name: app-config, items: [{key: app.conf, path: app.conf}]}}
    - {name: scripts, configMap: {name: app-config, defaultMode: 0755}}
    - {name: creds, secret: {secretName: app-secret}}
    - {name: scratch, emptyDir: {}}
    - {name: cache, emptyDir: {medium: Memory, sizeLimit: 64Mi}}
";

#[test]
fn configmaps_and_secrets_become_files_and_the_secrets_stay_out_of_the_manifest() {
    let c = conv(CONFIG);
    assert_eq!(names(&c), ["app-init-migrate", "app"]);
    let s = svc(&c, "app");
    let env = s.getn("environment");
    assert_eq!(pstr(env.getn("CFG_LOG_LEVEL")), "debug");
    // the secret, and the value made from it, are in the env file only
    assert!(env.get("DB_PASSWORD").is_none() && env.get("DSN").is_none());
    assert_eq!(
        pstr(s.getn("x-hydo-env-file")),
        "k8s-import/secrets/app.env"
    );
    let ef = c
        .files
        .iter()
        .find(|f| f.path.ends_with("secrets/app.env"))
        .unwrap();
    let t = String::from_utf8_lossy(&ef.bytes);
    assert!(t.contains("DB_PASSWORD=hunter2\n"), "{t}");
    assert!(
        t.contains("DSN=postgres://app:hunter2@db:5432/app\n"),
        "{t}"
    );
    assert_eq!(ef.mode, 0o600);
    assert!(!repr(&c.doc).contains("hunter2"));
    assert!(warned(
        &c,
        "ConfigMap nowhere (key x, variable MISSING) is not in the manifests"
    ));
    // the volumes
    let vols = as_list(s.get("volumes"));
    let by = |t: &str| {
        vols.iter()
            .find(|v| pstr(v.getn("target")) == t)
            .unwrap()
            .clone()
    };
    let cfg = by("/etc/app");
    assert_eq!(pstr(cfg.getn("type")), "bind");
    assert!(pstr(cfg.getn("source")).starts_with("/out/k8s-import/configmap/app-config-"));
    let sh = by("/start.sh");
    assert!(pstr(sh.getn("source")).ends_with("/start.sh"));
    let f = c
        .files
        .iter()
        .find(|f| f.path.ends_with("start.sh") && f.mode == 0o755);
    assert!(f.is_some(), "defaultMode 0755 is kept");
    assert_eq!(
        pstr(by("/run/creds").getn("source")),
        "/out/k8s-import/secrets/app-secret"
    );
    assert!(c
        .files
        .iter()
        .any(|f| f.path.ends_with("secrets/app-secret/token") && f.bytes == b"abc"));
    assert_eq!(pstr(by("/scratch").getn("source")), "app-scratch");
    assert_eq!(pstr(by("/cache").getn("type")), "tmpfs");
    assert_eq!(pstr(by("/cache").getn("tmpfs").getn("size")), "64m");
    // the init container is a one-shot job the pod waits for
    let init = svc(&c, "app-init-migrate");
    assert_eq!(pstr(init.getn("x-hydo-job")), "init");
    assert_eq!(
        pstr(
            s.getn("depends_on")
                .getn("app-init-migrate")
                .getn("condition")
        ),
        "service_completed_successfully"
    );
}

#[test]
fn dependencies_follow_the_names_a_container_uses() {
    let y = format!(
        "{DEPLOY}---
apiVersion: apps/v1
kind: Deployment
metadata: {{name: worker}}
spec:
  selector: {{matchLabels: {{app: worker}}}}
  template:
    metadata: {{labels: {{app: worker}}}}
    spec:
      containers:
        - name: worker
          image: example/worker:1
          env: [{{name: TARGET, value: \"http://web.default.svc.cluster.local:80/\"}}]
"
    );
    let c = conv(&y);
    let w = svc(&c, "worker");
    assert_eq!(
        pstr(w.getn("depends_on").getn("web").getn("condition")),
        "service_healthy"
    );
    assert!(svc(&c, "web").get("depends_on").is_none());
    // no probe and no port: ready is `true`, with a warning
    assert_eq!(pstr(w.getn("x-hydo-ready")), "true");
}

#[test]
fn jobs_cronjobs_and_helm_hooks() {
    let y = "apiVersion: batch/v1
kind: Job
metadata: {name: seed}
spec: {template: {spec: {restartPolicy: Never, containers: [{name: seed, image: example/tool:1, args: [seed]}]}}}
---
apiVersion: batch/v1
kind: CronJob
metadata: {name: nightly}
spec:
  schedule: \"0 3 * * *\"
  jobTemplate: {spec: {template: {spec: {containers: [{name: n, image: example/tool:1}]}}}}
---
apiVersion: batch/v1
kind: Job
metadata: {name: schema, annotations: {helm.sh/hook: \"pre-install,pre-upgrade\"}}
spec: {template: {spec: {containers: [{name: s, image: example/tool:1}]}}}
---
apiVersion: v1
kind: Pod
metadata: {name: smoke, annotations: {helm.sh/hook: test}}
spec: {containers: [{name: t, image: example/tool:1}]}
---
apiVersion: v1
kind: Pod
metadata: {name: api}
spec: {containers: [{name: api, image: example/api:1, ports: [{containerPort: 80}]}]}
";
    let c = conv(y);
    // the pre-install hook comes first, the Helm test is left out
    assert_eq!(names(&c), ["schema", "seed", "nightly", "api"]);
    assert_eq!(pstr(svc(&c, "seed").getn("x-hydo-job")), "provision");
    assert_eq!(pstr(svc(&c, "nightly").getn("x-hydo-job")), "cron");
    assert_eq!(pstr(svc(&c, "schema").getn("x-hydo-job")), "init");
    assert_eq!(
        pstr(
            svc(&c, "api")
                .getn("depends_on")
                .getn("schema")
                .getn("condition")
        ),
        "service_completed_successfully"
    );
    assert!(c.notes.iter().any(|n| n.contains("Pod smoke: a Helm test")));
    assert!(c.notes.iter().any(|n| n.contains("\"0 3 * * *\"")));
}

#[test]
fn what_does_not_map_is_one_line_each() {
    let y = format!(
        "{DEPLOY}---
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata: {{name: web}}
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {{name: deny-all}}
---
apiVersion: example.com/v1
kind: KafkaCluster
metadata: {{name: events}}
---
apiVersion: v1
kind: ServiceAccount
metadata: {{name: web}}
---
apiVersion: v1
kind: Service
metadata: {{name: external}}
spec: {{type: ExternalName, externalName: db.example.com}}
"
    );
    let c = conv(&y);
    assert!(warned(
        &c,
        "Ingress web: not imported; there is no ingress controller"
    ));
    assert!(warned(&c, "NetworkPolicy deny-all: not imported"));
    assert!(warned(
        &c,
        "KafkaCluster events (apiVersion example.com/v1): a custom resource"
    ));
    assert!(warned(
        &c,
        "Service external is an ExternalName (db.example.com)"
    ));
    assert!(c
        .notes
        .iter()
        .any(|n| n.starts_with("ServiceAccount web: ignored")));
}

#[test]
fn mesh_sidecars_are_left_out_and_other_containers_warned() {
    let y = "apiVersion: v1
kind: Pod
metadata: {name: p}
spec:
  containers:
    - {name: main, image: example/a:1}
    - {name: istio-proxy, image: istio/proxyv2:1}
    - {name: logs, image: example/b:1}
";
    let c = conv(y);
    assert_eq!(names(&c), ["p", "p-logs"]);
    assert!(warned(
        &c,
        "container istio-proxy belongs to a service mesh"
    ));
    assert!(warned(&c, "share localhost in Kubernetes"));
}

#[test]
fn exec_probes_run_inside_the_image() {
    let y = "apiVersion: v1
kind: Pod
metadata: {name: p}
spec:
  containers:
    - name: c
      image: example/a:1
      startupProbe: {exec: {command: [sh, -c, 'test -f $HOME/ok']}, initialDelaySeconds: 5}
";
    let c = conv(y);
    let s = svc(&c, "p");
    let h = s.getn("healthcheck");
    assert_eq!(
        list(h.getn("test")),
        ["CMD", "sh", "-c", "test -f $$HOME/ok"]
    );
    assert_eq!(pstr(h.getn("start_period")), "5s");
    assert!(pstr(s.getn("x-hydo-ready-from")).contains("startupProbe"));
}

#[test]
fn helpers() {
    assert_eq!(base64_decode("aHVudGVyMg==").unwrap(), b"hunter2");
    assert_eq!(quantity_mib(&Y::s("1Gi")), Some(1024));
    assert_eq!(quantity_mib(&Y::s("128M")), Some(123));
    assert_eq!(
        expand("a $(X) $$(X) $(Y)", &[("X".into(), "1".into())]),
        ("a 1 $(X) $(Y)".to_string(), vec!["X".to_string()])
    );
    assert_eq!(
        shown_command(&[
            "helm".into(),
            "template".into(),
            "--set".into(),
            "auth.password=x".into()
        ]),
        "helm template --set auth.password=..."
    );
    let r = redacted(&[(
        "t".into(),
        "kind: Secret\nmetadata:\n  name: s\ndata:\n  k: c2VjcmV0\n---\nkind: ConfigMap\ndata:\n  a: b\n"
            .into(),
    )]);
    assert!(
        !r.contains("c2VjcmV0") && r.contains("a: b") && r.contains("name: s"),
        "{r}"
    );
}
