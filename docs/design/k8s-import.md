# Importing Kubernetes manifests (`hydo k8s`)

Many teams have no Docker Compose file. They deploy with Kubernetes manifests, a Helm chart, or a
Kustomize folder, and before Milestone 243 they had to write `hydo.yaml` by hand. `hydo k8s`
turns what they have into a starting `hydo.yaml`, as `hydo compose` does for a Compose file.
Kubernetes itself does not run in the guest. Each container of each pod becomes a hydo service,
and no cluster is ever contacted.

This page is the reference of the importer: how it works, the mapping of every field, what it
refuses, and what it cannot do yet. The commands are in `hydo guide workflow`.

## How it works

The importer has three steps.

1. It reads the manifests. A file holds one or more YAML documents, or a JSON object, and a
   `kind: List` is opened. A folder with `Chart.yaml` is rendered with `helm template`, and a
   folder with `kustomization.yaml` with `kubectl kustomize`. Any other folder gives every
   `.yaml`, `.yml` and `.json` file under it.
2. It converts the objects into one document in the shape of a Compose file. The document has
   `x-hydo-*` keys for what Compose cannot say, such as a readiness command, a group of services
   or an `env_file:`.
3. It hands the document to the Compose importer. That importer resolves and pins the images by
   digest, writes the lock file and `fetch-images.sh`, and writes the volumes, mounts, restart
   policies and one-shot jobs as `hydo compose` writes them.

The `x-hydo-*` keys are read only in this mode. A Compose file's own `x-` keys stay ignored, so
`hydo compose` writes the same files as before.

`hydo k8s` writes these files next to `hydo.yaml`:

| file | what it holds |
|---|---|
| `hydo-k8s.lock.json` | the images' digests, as `hydo-compose.lock.json` holds them |
| `fetch-images.sh` | copies every image by digest into `images/` |
| `k8s-import/configmap/<name>/` | one file per key of each ConfigMap that a pod mounts, with the volume's file mode |
| `k8s-import/secrets/` | the files of each Secret that a pod mounts, and `<service>.env` with the environment values that come from Secrets; a `.gitignore` of `*` keeps all of it out of version control |
| `k8s-import/manifests.yaml` | what `helm template` or `kubectl kustomize` printed, with every Secret's `data:` and `stringData:` left out (only for a rendered chart or Kustomize folder) |

## The mapping

### Workloads

| Kubernetes | hydo.yaml |
|---|---|
| Deployment, ReplicaSet, ReplicationController | one pod. `replicas` of more than 1 gives a warning: the pods are alike, and a Service name resolves to one address, without load balancing. |
| StatefulSet | one pod per replica, named as Kubernetes names them: `<name>-0`, `<name>-1`, and so on. With 2 or more replicas, a group of them under `groups:`, so that `K=<name>.majority@5+2` works. |
| StatefulSet `podManagementPolicy: Parallel` | the pods after the first get `start_after: started`. The default, OrderedReady, starts each pod once the one before it is ready, which is hydo's own default. |
| DaemonSet | one pod: hydo is one machine. |
| Pod | one pod. |
| Job | a one-shot job (`job: true`): its service idles, and its command runs once in `provision:`, after every service is ready. |
| CronJob | a one-shot job that hydo runs nowhere by itself. Its timetable is a note; the workload runs the command with `hydo-oci` where the test needs it. |
| Helm hook `pre-install`, `pre-upgrade` | a one-shot job that every pod waits for: its command runs in each pod's `setup:` before the pod starts. |
| Helm hook `post-install`, `post-upgrade` | a one-shot job in `provision:`. |
| Helm hook `test` | left out with a note: its command can become a check. |
| other Helm hooks (`pre-delete`, rollbacks) | left out with a note: hydo installs once. |

A service's name is the pod's name, with `_` for each `-` and `.` (`r-redis-master-0` is
`r_redis_master_0`). The Kubernetes names still resolve through `/etc/hosts`. When the manifests
span several namespaces, each name starts with its namespace.

### Containers

| Kubernetes | hydo.yaml |
|---|---|
| the pod's first container | the pod's service |
| every other container | a service `<pod>-<container>`, with a warning: in Kubernetes the containers of a pod share `localhost`, in hydo each has its own address |
| an init container | a one-shot job `<pod>-init-<container>`. Its command runs in the `setup:` of the pod's services, once per boot, in order. |
| an init container with `restartPolicy: Always` (a native sidecar) | a service, as any other container |
| a service mesh's container (`istio-proxy`, `linkerd-proxy`, and their init containers) | left out with a warning: no mesh runs in hydo, so services talk directly |
| `image` | `image:` and `image_ref:`, pinned by digest as `hydo compose` pins them; `--no-resolve` leaves the tag |
| `command`, `args` | `entrypoint:`, `cmd:`, with `$(VAR)` filled in from the container's environment and `$$(VAR)` kept as `$(VAR)` |
| `workingDir` | `workdir:` |
| `securityContext.runAsUser`, `runAsGroup` (the container's, else the pod's) | `user: "<uid>[:<gid>]"` |
| `restartPolicy` of the pod (`Always` by default) | `restart:`: `always`, `on-failure` or `no` |
| `resources.limits.memory`, else `requests.memory` | counted in the `guest.ram_mib` guess |
| `resources.limits.cpu` | a comment: a CPU limit's effect is a pause of the workload, `P=workload` |

### Environment

| Kubernetes | hydo.yaml |
|---|---|
| (every container) | `HOSTNAME`: the pod's host name, which is its name unless `spec.hostname` sets one |
| (every container, unless `enableServiceLinks: false`) | for each Service of the namespace with a cluster address: `<NAME>_SERVICE_HOST` (the address its name resolves to), `<NAME>_SERVICE_PORT` and `<NAME>_SERVICE_PORT_<PORT NAME>`, with the container's port, because hydo has no proxy between the two ports |
| `env[].value` | `env:`, with `$(VAR)` filled in from the variables before it |
| `envFrom[].configMapRef`, `env[].valueFrom.configMapKeyRef` | `env:`, from the ConfigMap in the manifests (with `prefix`) |
| `envFrom[].secretRef`, `env[].valueFrom.secretKeyRef` | `env_file: k8s-import/secrets/<service>.env`. A variable whose `$(VAR)` names a Secret's variable goes there too. |
| `fieldRef` `metadata.name`, `metadata.namespace` | the pod's name, its namespace |
| `fieldRef` `status.podIP`, `status.hostIP` | the service's `addr:`, the bridge's address `10.89.0.1` |
| `fieldRef` `spec.nodeName`, `metadata.uid`, `metadata.labels['<key>']` | `hydo`, `hydo-<pod>`, the label's value |
| `resourceFieldRef` `limits.memory`, `limits.cpu` | the container's limit in bytes, in whole CPUs |

A value that comes from a Secret never goes into `hydo.yaml`. The file `<service>.env` holds it,
`hydo build` reads it when it builds the image (MANIFEST.md, "Values from a file"), and
`k8s-import/secrets/.gitignore` keeps it out of version control. The values are still inside the
image, as every environment value is. A command that names a Secret's variable as `$(VAR)` keeps
the `$(VAR)` unexpanded, with a warning, because its value would otherwise be written into `cmd:`.

### Services and names

| Kubernetes | hydo.yaml |
|---|---|
| a Service with a selector | its name, `<name>.<namespace>`, `<name>.<namespace>.svc` and `<name>.<namespace>.svc.cluster.local` resolve to the first selected pod's service |
| a headless Service that is a StatefulSet's `serviceName` | each pod's own names resolve too: `<pod>.<service>` and its longer forms |
| the Service's `targetPort` | the container port that a Service sends to comes first, so it is the service's `port:`, which network faults act on |
| `port` different from `targetPort` | a warning: hydo has no proxy between them, so a client must use the target port |
| `NodePort`, `LoadBalancer` | a note: clients in the guest use the name |
| `ExternalName`, or no selector | a warning: the name does not resolve |
| every pod | its own name resolves |

The start order follows the names. A container whose environment, command, or mounted ConfigMap
names another pod's Service gets `depends_on` on that pod, so hydo starts it once that pod is
ready. Kubernetes has no start order. It restarts a pod that fails while its dependencies start,
and `restart: always` does the same in hydo. A pair of pods that name each other gets no order,
with a note.

### Readiness

| Kubernetes | `ready:` |
|---|---|
| `readinessProbe`, else `startupProbe`, `httpGet` | `wget -q -T <timeoutSeconds> -O /dev/null http://<addr>:<port><path>` from the guest |
| `httpGet` with `scheme: HTTPS` | `tcp:<port>`, because the guest's `wget` has no TLS |
| `tcpSocket`, `grpc` | `tcp:<port>` (MANIFEST.md, "Readiness by a connect") |
| `exec` | the command inside the image, as the container's user, as `hydo compose` writes a healthcheck |
| no probe | `tcp:<first port>`, or `true` with a warning when the container has no port |
| `initialDelaySeconds + periodSeconds × failureThreshold` | `ready_timeout:`, 60 s more, at least 120 s, and at most 600 s |
| `livenessProbe` | a comment: hydo does not restart a service whose probe fails |

### Volumes

| Kubernetes | hydo.yaml |
|---|---|
| `persistentVolumeClaim` | a directory `/data/vol/<claim>` on the data disk, the fault target. Pods that name one claim share it. |
| `volumeClaimTemplates` of a StatefulSet | `/data/vol/<template>-<pod>`, as Kubernetes names the claim |
| `emptyDir` | `/data/vol/<pod>-<volume>`, shared by the pod's containers and its init containers |
| `emptyDir` with `medium: Memory`, used by one container | a tmpfs, with `sizeLimit` as its size |
| `subPath` on a claim or an `emptyDir` | its own directory, `<volume>-<subPath>` |
| `configMap`, `secret`, `projected` (ConfigMap and Secret sources) | files under `k8s-import/`, copied into the image root and mounted read-only; `items`, `defaultMode` and `mode` are kept; a `subPath` mounts one file |
| `securityContext.fsGroup` | `setup:` gives the claims and `emptyDir`s that group and group write access before every start; `fsGroupChangePolicy: OnRootMismatch` only when the top directory's group differs |
| an empty claim or `emptyDir` | mode 0777 at its first start, as most volume provisioners make it |
| `hostPath` | an empty directory on the data disk, with a warning |
| `downwardAPI`, other volume types | skipped or an empty directory, with a warning |

A Kubernetes volume starts empty. A Docker named volume takes the image's content at its first
use, but these volumes do not (`copy_up` is not set). A claim mounted on `/data` follows the
Compose importer's rule: when several services have their own volume there, each gets its own
directory as `/data`.

## What it refuses

Each of these gives one warning line in the output and in the written file's header, with what to
do:

| Kubernetes | why |
|---|---|
| Ingress, IngressClass, Gateway, HTTPRoute | there is no ingress or gateway controller; the workload reaches the Services by their names |
| NetworkPolicy | isolation between pods is not modelled: every service reaches every other, and hydo's network faults cut links on purpose instead |
| CustomResourceDefinition and any custom resource | its operator does not run in hydo; import what the operator would create instead |
| admission webhooks | they do not run, so nothing they inject is added |
| Endpoints, EndpointSlice | hand-written endpoints point outside the guest |
| a service mesh's containers | no mesh runs in hydo |
| `affinity`, `nodeSelector`, `tolerations`, `topologySpreadConstraints` and the other scheduling keys | hydo runs every pod on one machine |
| `hostNetwork`, `hostPID`, `hostIPC` | each service has its own network namespace |
| `securityContext.sysctls`, `privileged` | not set; add the commands to `setup:` if the program needs them |
| `lifecycle.postStart`, `preStop` | not run |
| `hostAliases` | not added to `/etc/hosts` |

Some kinds are ignored with a note, because hydo needs nothing of them. They are
ServiceAccount, the RBAC kinds, PodDisruptionBudget, HorizontalPodAutoscaler, Namespace,
PersistentVolume, StorageClass, LimitRange, ResourceQuota, and PriorityClass.

## Charts and Kustomize folders

`hydo k8s <chart folder>` or `hydo k8s --helm <chart>` runs:

```sh
helm template <release> <chart> [--namespace NS] [--values FILE ...] [--set KEY=VALUE ...]
```

The release name is `--release`, by default `hydo`. The written file's header repeats the command
with every `--set` value replaced by `...`, because such a value can be a password. A chart that
makes up a random password when none is given gives another Secret at every import: give the
password with `--set` or a values file. Without `helm`, the import stops, and it prints the command
to render the chart by hand; `hydo k8s rendered.yaml` then imports the result.

A folder with `kustomization.yaml` is rendered with `kubectl kustomize <folder>`, else with
`kustomize build <folder>`.

## Limits

- One instance of a Deployment, whatever its `replicas`. A Service name resolves to one address,
  and nothing balances the load.
- The containers of a pod do not share `localhost`. A sidecar that a program reaches as
  `localhost:<port>` needs its name instead, or an import with `--network loopback`.
- A Service whose `port` differs from its `targetPort` is reached on the target port only.
- No Kubernetes API: a program that reads the API (leader election through leases, an operator, a
  controller) does not work. Such a program needs Kubernetes itself in the guest, which
  `docs/design/k3s-in-guest.md` runs with k3s on hvx (Milestone 246).
- Of the Service variables, the older link-style ones (`<NAME>_PORT=tcp://...` and
  `<NAME>_PORT_<port>_TCP_*`) are not set.
- Probes are read for readiness only.
- Network policies, quotas and scheduling are not modelled.

## The proof

The proof is in `docs/HISTORY.md`, Milestone 243.
