#!/usr/bin/env python3
"""hydo-compose -- turn a docker-compose.yaml into a starting hydo.yaml (MANIFEST.md, "From Docker
Compose").

    hydo-compose <docker-compose.yaml> [--out hydo.yaml] [--name NAME] [--images-dir DIR]
                 [--env-file FILE ...] [--profile P ...] [--use-env] [--no-resolve] [--lock FILE]
                 [--no-addr]

What it does, per Compose service (the table in MANIFEST.md lists every key):
  * image: resolved to digests with skopeo (the index digest and the linux/amd64 + linux/arm64
    manifest digests; tags are never trusted at build time). The image config (ports, healthcheck,
    volumes, user, CPU variant) is read with `skopeo inspect --config`. Everything resolved is
    recorded in a lock file (default hydo-compose.lock.json next to the output), so a second run
    needs no network and gives the same manifest. A script fetch-images.sh (next to the output)
    copies the images by digest into --images-dir (default images/) -- hydo-build never pulls.
  * command / entrypoint / environment / env_file / working_dir / user -> the service's cmd /
    entrypoint / env / workdir / user (Compose semantics: strings are split into words, $$ is $).
  * depends_on (incl. condition: service_healthy) -> depends_on (hydo starts a service only after
    its dependencies are ready, so service_healthy and service_started both hold);
    condition: service_completed_successfully (a one-shot job) -> the job runs in the setup of the
    services that depend on it.
  * healthcheck (or the image's HEALTHCHECK) -> ready: (run inside the image with hydo-oci);
    without one, a TCP check of the service's port.
  * ports / expose (else the image's EXPOSE) -> port: (the first container port).
  * every service gets its own loopback addr: (127.0.0.11, .12, ...) and the Compose service names
    (plus container_name, hostname, network aliases, links aliases, extra_hosts) resolve through
    /etc/hosts, in the hydo root and inside every image root. --no-addr: every name resolves to
    127.0.0.1 and no addr: is written (the arm64 build, `hydo compose` on a Mac, has no addr: yet).
  * volumes: named and anonymous volumes (and the image's VOLUMEs) -> directories on the data disk
    (/data/vol/<name>, the fault target) mounted into the image with a copy of the image's content
    on first use, as Docker does; bind mounts of existing files/directories -> copied into the
    read-only root and mounted; tmpfs -> tmpfs. Every image service also gets a writable /run and
    a /dev/shm (a container has them; the hydo image root is read-only).
  * restart -> recovery (restart: "no" = not started again after a power cut).
It cannot know the workload or the invariants: it writes a TODO skeleton for both, which the
hydo-onboard skill (`hydo guide`; skills/hydo-onboard in hydo's repository) fills in. Unsupported keys are reported as warnings
with what to do. Stdlib only; PyYAML is used when installed (anchors, merge keys), otherwise the
hydo-build YAML subset parser (or give a .json file, e.g. `docker compose config --format json`).
"""
import hashlib, importlib.machinery, importlib.util, json, os, posixpath, re, shlex, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ADDR_BASE = 11                      # 127.0.0.11, .12, ... (as examples/etcd)
SVC_RE = re.compile(r"^[a-z][a-z0-9_]*$")
PATH_OK = re.compile(r"^/[A-Za-z0-9._@+=,:/-]*$")

# known image families: (match on the repository's last path component, ram MiB, data MiB, cpu)
FAMILIES = [
    (r"^(postgres|postgis|timescaledb)", 384, 1024, None),
    (r"^(mysql|mariadb|percona)", 640, 1024, None),
    (r"^(redis|valkey|keydb)", 96, 256, None),
    (r"^(mongo)", 640, 1024, None),
    (r"^(kafka|cp-kafka|zookeeper|cp-zookeeper|elasticsearch|opensearch|cassandra|keycloak)", 1024, 1024, None),
    (r"^(rabbitmq)", 384, 512, None),
    (r"^(memcached|nginx|haproxy|traefik|caddy)", 64, 128, None),
    (r"^restate$", 512, 1024, "v3"),     # restatedev/restate: built with target-cpu=broadwell (M68)
]
DEFAULT_SVC_RAM = 256

# keys of a Compose service and what the importer does with them
HANDLED = {"image", "command", "entrypoint", "environment", "env_file", "working_dir", "user",
           "depends_on", "healthcheck", "ports", "expose", "volumes", "restart", "container_name",
           "hostname", "networks", "extra_hosts", "links", "tmpfs", "profiles", "build", "deploy",
           "mem_limit", "mem_reservation", "secrets", "platform"}
IGNORED = {"labels", "logging", "stdin_open", "tty", "stop_signal", "stop_grace_period", "domainname",
           "pull_policy", "annotations", "attach", "cpu_count", "cpu_percent", "cpu_shares", "cpus",
           "cpu_quota", "cpu_period", "cpuset", "oom_score_adj", "oom_kill_disable", "x-*"}
UNSUPPORTED = {
    "privileged": "hydo runs every service as a plain process (no capabilities beyond root's); remove it "
                  "if the program does not need it",
    "cap_add": "capabilities are not modelled; programs run as root or as their user",
    "cap_drop": "capabilities are not modelled",
    "devices": "host devices are not available in the guest; drop the service or replace the device",
    "device_cgroup_rules": "host devices are not available in the guest",
    "security_opt": "seccomp/AppArmor profiles are not applied",
    "network_mode": "all services share one loopback network in the guest; network_mode is ignored",
    "pid": "pid namespaces are not modelled; services see each other's processes",
    "ipc": "ipc namespaces are not modelled",
    "uts": "uts namespaces are not modelled (the guest has one hostname)",
    "userns_mode": "user namespaces are not modelled",
    "cgroup": "cgroups are not modelled",
    "cgroup_parent": "cgroups are not modelled",
    "sysctls": "set them in a service's setup: (sysctl -w ...) if the program needs them",
    "ulimits": "set them in a service's setup: (ulimit) if the program needs them",
    "dns": "there is no DNS in the guest; names resolve through /etc/hosts",
    "dns_search": "there is no DNS in the guest",
    "dns_opt": "there is no DNS in the guest",
    "init": "there is no init process per service (no tini); a program that must reap children "
            "should be started by its own init",
    "volumes_from": "list the volumes explicitly in each service",
    "extends": "inline the extended service (or run `docker compose config` and import its output)",
    "configs": "use a bind mount of the file instead",
    "scale": "one instance per service; copy the service to get more",
    "isolation": "not modelled",
    "shm_size": "every image service gets a 64 MiB /dev/shm",
    "group_add": "add the groups in the image's /etc/group",
    "runtime": "not modelled",
    "storage_opt": "not modelled",
    "mac_address": "not modelled",
    "post_start": "add the commands to the service's setup: or to provision:",
    "pre_stop": "not modelled",
    "develop": "not modelled",
    "gpus": "no GPUs in the guest",
}
SPECIAL_HOST = ("/var/run/docker.sock", "/run/docker.sock", "/etc/localtime", "/etc/timezone", "/proc",
                "/sys", "/dev", "/var/lib/docker", "/lib/modules")


class ComposeError(Exception):
    pass


# ------------------------------------------------------------------ loading
def _hydo_build():
    loader = importlib.machinery.SourceFileLoader("hydo_build_mod", os.path.join(HERE, "hydo-build"))
    spec = importlib.util.spec_from_loader("hydo_build_mod", loader)
    m = importlib.util.module_from_spec(spec)
    loader.exec_module(m)
    return m


def load_yaml_text(text, name):
    if name.endswith(".json"):
        return json.loads(text)
    try:
        import yaml                 # PyYAML when installed: anchors, aliases, merge keys
        return yaml.safe_load(text)
    except ImportError:
        pass
    hb = _hydo_build()
    try:
        return resolve_anchors(hb.parse_yaml(anchors_to_keys(text, name), os.path.basename(name)), name)
    except hb.YamlError as e:
        raise ComposeError("%s (the stdlib parser supports the hydo.yaml subset only: no anchors, aliases "
                           "or merge keys. Install PyYAML, or convert the file first: "
                           "`docker compose -f %s config --format json > compose.json`)" % (e, name))


ANCHOR_KEY = "__hydo_anchor__"
ALIAS_PFX = "__hydo_alias__ "
_ANCH = re.compile(r"^(\s*(?:- )?[^#'\"]*?:)\s+&([A-Za-z0-9_.-]+)\s*(#.*)?$")
_ALIAS = re.compile(r"(:\s+|^\s*-\s+|[\[,]\s*)\*([A-Za-z0-9_.-]+)(?=\s*(?:$|#|,|\]))")


def anchors_to_keys(text, name):
    """Without PyYAML: rewrite the common Compose uses of anchors into the hydo-build subset.
    `key: &a` (a block mapping follows) -> `key:` + a first child `__hydo_anchor__: a`;
    `*a` as a value, list item or flow item -> the string "__hydo_alias__ a" (resolved after
    parsing by resolve_anchors, `<<:` merge keys included). Anchors on scalars or sequences are
    refused (use PyYAML or `docker compose config --format json`)."""
    if "&" not in text and "*" not in text:
        return text
    lines = text.split("\n")
    out = []
    for i, ln in enumerate(lines):
        mo = _ANCH.match(ln)
        if mo:
            nxt = next((x for x in lines[i + 1:] if x.strip() and not x.strip().startswith("#")), "")
            ind = len(nxt) - len(nxt.lstrip())
            base = len(mo.group(1)) - len(mo.group(1).lstrip())
            if ind <= base or nxt.lstrip().startswith("- "):
                raise ComposeError("%s:%d: anchor &%s is not on a block mapping; the stdlib parser handles only "
                                   "those (install PyYAML or import `docker compose config --format json`)"
                                   % (name, i + 1, mo.group(2)))
            out.append(mo.group(1))
            out.append(" " * ind + "%s: %s" % (ANCHOR_KEY, mo.group(2)))
            continue
        if re.search(r"(^|[\s:])&[A-Za-z0-9_.-]+", ln.split("#", 1)[0]) and ":" in ln and "&" in ln.split(":", 1)[1].split("#")[0][:3]:
            raise ComposeError("%s:%d: anchors on scalars or flow values need PyYAML (or import `docker compose "
                               "config --format json`)" % (name, i + 1))
        out.append(_ALIAS.sub(lambda m: '%s"%s%s"' % (m.group(1), ALIAS_PFX, m.group(2)), ln))
    return "\n".join(out)


def resolve_anchors(doc, name):
    anchors = {}

    def collect(x):
        if isinstance(x, dict):
            if ANCHOR_KEY in x:
                anchors[str(x.pop(ANCHOR_KEY))] = x
            for v in x.values():
                collect(v)
        elif isinstance(x, list):
            for v in x:
                collect(v)
    collect(doc)
    if not anchors and ALIAS_PFX not in json.dumps(doc):
        return doc

    def res(x, depth=0):
        if depth > 50:
            raise ComposeError("%s: anchors nest too deep (a cycle?)" % name)
        if isinstance(x, str) and x.startswith(ALIAS_PFX):
            k = x[len(ALIAS_PFX):]
            if k not in anchors:
                raise ComposeError("%s: alias *%s has no anchor &%s (only block-mapping anchors are read "
                                   "without PyYAML)" % (name, k, k))
            return res(json.loads(json.dumps(anchors[k])), depth + 1)
        if isinstance(x, list):
            return [res(v, depth) for v in x]
        if isinstance(x, dict):
            out = {}
            if "<<" in x:
                for m in as_list(res(x["<<"], depth)):
                    if isinstance(m, dict):
                        out.update(m)
            for k, v in x.items():
                if k != "<<":
                    out[k] = res(v, depth)
            return out
        return x
    return res(doc)


def parse_env_file(path, required=True):
    """Docker/Compose env file: KEY=VALUE lines, # comments, optional `export `, quotes stripped."""
    if not os.path.isfile(path):
        if required:
            raise ComposeError("env file %s does not exist" % path)
        return {}, []
    out, bare = {}, []
    for ln in open(path, encoding="utf-8").read().splitlines():
        s = ln.strip()
        if not s or s.startswith("#"):
            continue
        if s.startswith("export "):
            s = s[7:].lstrip()
        k, sep, v = s.partition("=")
        k = k.strip()
        if not sep:
            bare.append(k)
            continue
        v = v.strip()
        if len(v) >= 2 and v[0] == v[-1] and v[0] in "'\"":
            v = v[1:-1]
            if ln.strip().split("=", 1)[1].strip()[0] == '"':
                v = v.replace("\\n", "\n").replace('\\"', '"')
        elif " #" in v:
            v = v.split(" #", 1)[0].rstrip()
        out[k] = v
    return out, bare


_VAR = re.compile(r"\$(?:\$|\{([^}]*)\}|([A-Za-z_][A-Za-z0-9_]*))")


class Interp:
    """Compose variable interpolation: $VAR ${VAR} ${VAR:-d} ${VAR-d} ${VAR:?e} ${VAR?e} ${VAR:+r}
    ${VAR+r}; $$ is a literal $ (kept as $$ here, unescaped where Compose unescapes)."""

    def __init__(self, env, shell_env, use_env, warn):
        self.env, self.shell, self.use, self.warn = env, shell_env, use_env, warn
        self.used_shell = set()

    def get(self, k):
        if self.use and k in self.shell:
            return self.shell[k]
        if k in self.env:
            if k in self.shell and self.shell[k] != self.env[k] and k not in self.used_shell:
                self.used_shell.add(k)
                self.warn("interpolation", "${%s} is also set in your shell environment; Compose would use the "
                          "shell's value, hydo-compose used the .env value (pass --use-env to follow Compose)" % k)
            return self.env[k]
        if k in self.shell and k not in self.used_shell:
            self.used_shell.add(k)
            self.warn("interpolation", "${%s} is set only in your shell environment; it was NOT used (the "
                      "manifest must not depend on who runs the importer). Put it in .env or pass --use-env" % k)
        return None

    def sub(self, s, where):
        def one(mo):
            if mo.group(0) == "$$":
                return "$$"
            body = mo.group(1) if mo.group(1) is not None else mo.group(2)
            m2 = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)(?:(:?[-?+])(.*))?$", body, re.S)
            if not m2:
                raise ComposeError("%s: invalid interpolation ${%s}" % (where, body))
            k, op, arg = m2.group(1), m2.group(2), m2.group(3) or ""
            v = self.get(k)
            if op is None:
                if v is None:
                    self.warn("interpolation", "%s: ${%s} is not set; it became an empty string (as in Compose)" % (where, k))
                    return ""
                return v
            unset = v is None or (op.startswith(":") and v == "")
            o = op[-1]
            if o == "-":
                return self.sub(arg, where) if unset else v
            if o == "?":
                if unset:
                    raise ComposeError("%s: required variable %s is not set: %s" % (where, k, arg or "missing"))
                return v
            return "" if unset else self.sub(arg, where)
        return _VAR.sub(one, s)

    def walk(self, x, where="compose"):
        if isinstance(x, str):
            return self.sub(x, where)
        if isinstance(x, list):
            return [self.walk(y, where) for y in x]
        if isinstance(x, dict):
            return {k: self.walk(v, "%s.%s" % (where, k)) for k, v in x.items()}
        return x


def unesc(s):
    return s.replace("$$", "$") if isinstance(s, str) else s


# ------------------------------------------------------------------ images
def parse_ref(ref):
    """'redis:7.2.4', 'ghcr.io/x/y@sha256:..' -> (registry, repo, tag, digest, display)"""
    r = ref.strip()
    digest = None
    if "@" in r:
        r, digest = r.split("@", 1)
    tag = None
    last = r.rsplit("/", 1)[-1]
    if ":" in last:
        r, tag = r.rsplit(":", 1)
    parts = r.split("/")
    if len(parts) > 1 and ("." in parts[0] or ":" in parts[0] or parts[0] == "localhost"):
        reg, repo = parts[0], "/".join(parts[1:])
    else:
        reg, repo = "docker.io", r
    if reg == "docker.io" and "/" not in repo:
        repo = "library/" + repo
    if tag is None and digest is None:
        tag = "latest"
    return reg, repo, tag, digest


def short_name(reg, repo):
    r = repo[len("library/"):] if reg == "docker.io" and repo.startswith("library/") else repo
    return r if reg == "docker.io" else "%s/%s" % (reg, r)


def file_stem(reg, repo, tag, digest):
    base = short_name(reg, repo).replace("/", "-")
    base = re.sub(r"[^A-Za-z0-9._-]", "-", base)
    t = tag or digest.split(":")[1][:12]
    return "%s-%s" % (base, re.sub(r"[^A-Za-z0-9._-]", "-", t))


def find_skopeo():
    for c in (os.environ.get("SKOPEO"), shutil.which("skopeo")):
        if c and os.access(c, os.X_OK):
            return c
    import glob
    for c in sorted(glob.glob("/nix/store/*-skopeo-*/bin/skopeo")):
        if os.access(c, os.X_OK):
            return c
    if shutil.which("nix"):
        try:
            out = subprocess.run(["nix", "build", "--no-link", "--print-out-paths", "nixpkgs#skopeo"],
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=600).stdout
            for p in out.split():
                if os.access(os.path.join(p, "bin/skopeo"), os.X_OK):
                    return os.path.join(p, "bin/skopeo")
        except (OSError, subprocess.SubprocessError):
            pass
    return None


def _skopeo(sk, args):
    r = subprocess.run([sk] + args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=300)
    if r.returncode != 0:
        raise ComposeError("skopeo %s: %s" % (" ".join(args), r.stderr.decode("utf-8", "replace").strip()[-400:]))
    return r.stdout


INDEX_TYPES = ("application/vnd.oci.image.index.v1+json",
               "application/vnd.docker.distribution.manifest.list.v2+json")


def resolve_image(sk, ref):
    """-> lock entry {ref, name, index_digest, platforms: {amd64: {digest, variant, config}, arm64: ...}}"""
    reg, repo, tag, digest = parse_ref(ref)
    base = "docker://%s/%s" % (reg, repo)
    raw = _skopeo(sk, ["inspect", "--raw", "%s%s" % (base, "@" + digest if digest else ":" + tag)])
    top = "sha256:" + hashlib.sha256(raw).hexdigest()
    if digest and digest != top:
        raise ComposeError("%s: the registry's manifest has digest %s, not %s" % (ref, top, digest))
    doc = json.loads(raw)
    plats = {}
    if doc.get("mediaType") in INDEX_TYPES or "manifests" in doc:
        cands = {}
        for mf in doc.get("manifests") or []:
            p = mf.get("platform") or {}
            if p.get("os") != "linux" or p.get("architecture") not in ("amd64", "arm64"):
                continue
            cands.setdefault(p["architecture"], []).append((p.get("variant") or "", mf["digest"]))
        for arch, lst in cands.items():
            if arch == "amd64":
                pref = [x for x in lst if x[0] in ("", "v1", "v2")] or sorted(lst)
            else:
                pref = [x for x in lst if x[0] in ("", "v8")] or sorted(lst)
            plats[arch] = {"digest": pref[0][1], "variant": pref[0][0],
                           "variants": sorted(v for v, _ in lst)}
    else:
        plats[None] = {"digest": top, "variant": ""}
    out = {}
    for arch, p in plats.items():
        cfg = json.loads(_skopeo(sk, ["inspect", "--config", "--raw", "%s@%s" % (base, p["digest"])]))
        a = arch or cfg.get("architecture")
        if a not in ("amd64", "arm64"):
            continue
        c = cfg.get("config") or {}
        out[a] = {"digest": p["digest"], "variant": p.get("variant") or cfg.get("variant") or "",
                  "variants": p.get("variants") or [],
                  "config": {k: c.get(k) for k in ("Entrypoint", "Cmd", "Env", "WorkingDir", "User",
                                                    "ExposedPorts", "Healthcheck", "Volumes")}}
    if "amd64" not in out:
        raise ComposeError("%s: no linux/amd64 image (found: %s)" % (ref, ", ".join(sorted(out)) or "none"))
    return {"ref": ref, "name": short_name(reg, repo), "tag": tag, "index_digest": top,
            "registry": reg, "repo": repo, "platforms": out}


def config_from_file(path, arch):
    """The image config of a local image file (docker save tar / OCI layout), via hydo_oci."""
    sys.path.insert(0, HERE)
    import hydo_oci
    img = hydo_oci.Image(path, None, {"amd64": "x86_64", "arm64": "arm64"}[arch])
    c = img.config.get("config") or {}
    return {"digest": img.manifest_digest or img.config_digest, "variant": img.config.get("variant") or "",
            "variants": [], "config": {k: c.get(k) for k in ("Entrypoint", "Cmd", "Env", "WorkingDir", "User",
                                                             "ExposedPorts", "Healthcheck", "Volumes")}}


# ------------------------------------------------------------------ YAML output (hydo-build's subset)
PLAIN = re.compile(r"^[A-Za-z0-9_./][A-Za-z0-9_./@+=-]*$")


def q(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    s = str(v)
    if PLAIN.match(s) and s not in ("true", "false", "yes", "no", "null", "True", "False", "~") \
            and not re.fullmatch(r"-?\d+", s):
        return s
    return json.dumps(s)


def flow_list(xs):
    return "[" + ", ".join(q(x) for x in xs) + "]"


def flow_map(d):
    return "{" + ", ".join("%s: %s" % (k, q(v)) for k, v in d.items()) + "}"


def block(text, indent):
    pad = " " * indent
    return "|\n" + "".join((pad + ln if ln else "") + "\n" for ln in text.rstrip("\n").split("\n"))


def comment(text, indent=0):
    pad = " " * indent
    return "".join("%s# %s\n" % (pad, ln) if ln else "%s#\n" % pad for ln in text.split("\n"))


# ------------------------------------------------------------------ the conversion
def svc_name(n):
    s = re.sub(r"[^a-z0-9_]", "_", str(n).lower())
    if not s or not s[0].isalpha():
        s = "s_" + s
    return s


def as_list(x):
    if x is None:
        return []
    return x if isinstance(x, list) else [x]


def env_of(v, where, warn, interp_env, use_env):
    """environment: list KEY=VAL / KEY, or mapping -> ordered dict (values str)."""
    out = {}
    if v is None:
        return out
    items = []
    if isinstance(v, dict):
        items = list(v.items())
    elif isinstance(v, list):
        for e in v:
            k, sep, val = str(e).partition("=")
            items.append((k, val if sep else None))
    else:
        raise ComposeError("%s: environment must be a list or a mapping" % where)
    for k, val in items:
        k = str(k)
        if val is None:     # Compose: from the project environment (.env, the shell)
            v = interp_env.get(k) if interp_env else None
            if v is not None:
                out[k] = v
            else:
                warn("environment", "%s: %s has no value and is not in .env (Compose would take it from the "
                     "shell); it was left out -- give it a value in .env, or pass --use-env" % (where, k))
            continue
        out[k] = "" if val is None else (str(val).lower() if isinstance(val, bool) else str(val))
    return out


def words(v, where):
    """command/entrypoint: list as is; string split into words as Compose does (no shell)."""
    if v is None:
        return None
    if isinstance(v, list):
        return [unesc(str(x)) for x in v]
    try:
        return [unesc(x) for x in shlex.split(str(v))]
    except ValueError as e:
        raise ComposeError("%s: cannot split %r into words (%s)" % (where, v, e))


def container_ports(svc, where, warn):
    out = []
    for p in as_list(svc.get("ports")):
        if isinstance(p, dict):
            t, proto = p.get("target"), p.get("protocol", "tcp")
        else:
            s = str(p)
            proto = "tcp"
            if "/" in s:
                s, proto = s.rsplit("/", 1)
            t = s.rsplit(":", 1)[-1]
        if proto != "tcp":
            warn("ports", "%s: %s port %s: network faults act on TCP/UDP to that port alike; fine" % (where, proto, t))
        rng = str(t).split("-")
        try:
            out.append(int(rng[0]))
        except ValueError:
            warn("ports", "%s: cannot read the container port of %r; ignored" % (where, p))
            continue
        if len(rng) > 1:
            warn("ports", "%s: port range %s: network faults act on the first port only (%s)" % (where, t, rng[0]))
    for p in as_list(svc.get("expose")):
        try:
            out.append(int(str(p).split("/")[0].split("-")[0]))
        except ValueError:
            warn("expose", "%s: cannot read %r; ignored" % (where, p))
    return out


def duration_s(v, default):
    """Compose durations: 30s, 1m30s, 500ms, 1h -> seconds (int, at least 1)."""
    if v is None:
        return default
    if isinstance(v, int):
        return max(1, v // 1000000000) if v > 100000 else v
    tot, s = 0.0, str(v).strip()
    for num, unit in re.findall(r"([\d.]+)(us|ms|s|m|h)?", s):
        tot += float(num) * {"us": 1e-6, "ms": 1e-3, "s": 1, "m": 60, "h": 3600, "": 1}[unit or ""]
    return max(1, int(round(tot)))


def mem_mib(v):
    if v is None:
        return None
    if isinstance(v, int):
        return v // (1 << 20)
    mo = re.fullmatch(r"\s*([\d.]+)\s*([bkmg]?)i?b?\s*", str(v).lower())
    if not mo:
        return None
    n = float(mo.group(1))
    return int(n * {"": 1.0 / (1 << 20), "b": 1.0 / (1 << 20), "k": 1.0 / 1024, "m": 1, "g": 1024}[mo.group(2)])


class Importer:
    def __init__(self, compose_path, out_path, opts):
        self.cpath = os.path.abspath(compose_path)
        self.cdir = os.path.dirname(self.cpath)
        self.out = os.path.abspath(out_path)
        self.odir = os.path.dirname(self.out)
        self.o = opts
        self.warnings = []          # (key, text)
        self.notes = []

    def warn(self, key, text):
        if (key, text) not in self.warnings:
            self.warnings.append((key, text))

    # -------------------------------------------------------------- top
    def load(self):
        text = open(self.cpath, encoding="utf-8").read()
        doc = load_yaml_text(text, self.cpath)
        if not isinstance(doc, dict) or not isinstance(doc.get("services"), dict) or not doc["services"]:
            raise ComposeError("%s: no services: mapping (is this a Compose file?)" % self.cpath)
        envf = {}
        dot = os.path.join(self.cdir, ".env")
        files = self.o.get("env_files") or ([dot] if os.path.isfile(dot) else [])
        for f in files:
            e, _ = parse_env_file(f)
            envf.update(e)
        self.interp = Interp(envf, dict(os.environ), self.o.get("use_env"), self.warn)
        self.doc = self.interp.walk(doc)
        for k in self.doc:
            if k in ("services", "volumes", "networks", "name", "version", "secrets") or str(k).startswith("x-"):
                continue
            if k == "configs":
                self.warn("configs", "top-level configs: not supported; use bind mounts of the files")
            elif k == "include":
                self.warn("include", "include: is not followed; merge the files first (`docker compose config`)")
            else:
                self.warn(k, "top-level %s: ignored" % k)
        if "version" in self.doc:
            self.notes.append("version: %s ignored (the Compose specification has no versions)" % self.doc["version"])

    def lock_load(self):
        p = self.o.get("lock") or os.path.join(self.odir, "hydo-compose.lock.json")
        self.lock_path = p
        try:
            self.lock = json.load(open(p)).get("images", {})
        except (OSError, ValueError):
            self.lock = {}
        self.lock_dirty = False

    def image_info(self, ref):
        if ref in self.lock:
            return self.lock[ref]
        reg, repo, tag, digest = parse_ref(ref)
        stem = file_stem(reg, repo, tag, digest)
        idir = os.path.join(self.odir, self.o.get("images_dir") or "images")
        amd = os.path.join(idir, stem + "-amd64.docker.tar")
        if os.path.exists(amd):            # already fetched: read its config, no network
            info = {"ref": ref, "name": short_name(reg, repo), "tag": tag, "index_digest": digest,
                    "registry": reg, "repo": repo, "platforms": {"amd64": config_from_file(amd, "amd64")},
                    "from": "local file"}
            arm = os.path.join(idir, stem + "-arm64-oci")
            if os.path.exists(arm):
                info["platforms"]["arm64"] = config_from_file(arm, "arm64")
            return info
        if self.o.get("no_resolve"):
            return {"ref": ref, "name": short_name(reg, repo), "tag": tag, "index_digest": digest,
                    "registry": reg, "repo": repo, "platforms": {}, "unresolved": True}
        if not hasattr(self, "sk"):
            self.sk = find_skopeo()
        if not self.sk:
            raise ComposeError("skopeo not found (set SKOPEO, install it, or have nix); it resolves %s to a digest. "
                               "Use --no-resolve to write an unpinned manifest" % ref)
        print("hydo-compose: resolving %s" % ref, file=sys.stderr)
        info = resolve_image(self.sk, ref)
        self.lock[ref] = info
        self.lock_dirty = True
        return info

    # -------------------------------------------------------------- services
    def run(self):
        self.load()
        self.lock_load()
        prof = set(self.o.get("profiles") or [])
        raw = self.doc["services"]
        order = []
        for n, s in raw.items():
            s = s or {}
            ps = as_list(s.get("profiles"))
            if ps and not (prof & set(map(str, ps))):
                self.warn("profiles", "service %s is in profile(s) %s and was left out (as `docker compose up` "
                          "does); pass --profile %s to include it" % (n, ", ".join(map(str, ps)), ps[0]))
                continue
            order.append(n)
        if not order:
            raise ComposeError("no service left after profiles")
        names = {}
        for n in order:
            hn = svc_name(n)
            if hn in names.values():
                raise ComposeError("services %s and %s map to the same hydo name %s; rename one" %
                                   (n, [k for k, v in names.items() if v == hn][0], hn))
            names[n] = hn
            if hn != n:
                self.notes.append("Compose service %r is hydo service %r (fault tokens use %s; the name %s still "
                                  "resolves through /etc/hosts)" % (n, hn, hn, n))
        if len(order) > 250 - ADDR_BASE:
            raise ComposeError("too many services for one loopback /24")
        self.names = names
        if self.o.get("no_addr"):      # --no-addr (the arm64 build has no addr: yet): every name -> 127.0.0.1
            self.addr = {n: "127.0.0.1" for n in order}
        else:
            self.addr = {n: "127.0.0.%d" % (ADDR_BASE + i) for i, n in enumerate(order)}
        self.hosts = []
        self.bind_files = []           # (src abs, guest path)
        self.data_dirs = []
        self.ram = 256                 # the guest kernel + hydo's init
        self.data_mib = 256
        self.cpu = None
        self.svcs = []
        jobs = self.find_jobs(order, raw)
        for n in order:
            self.svcs.append(self.service(n, raw[n] or {}, jobs))
        self.volumes_top = self.doc.get("volumes") or {}
        for v, d in (self.volumes_top or {}).items():
            if isinstance(d, dict) and d.get("external"):
                self.warn("volumes", "volume %s is external: it starts empty in the guest (no data is imported)" % v)
            if isinstance(d, dict) and d.get("driver") not in (None, "local"):
                self.warn("volumes", "volume %s: driver %s ignored; it is a directory on the data disk" % (v, d["driver"]))
        nets = self.doc.get("networks") or {}
        custom = [k for k in nets if k != "default"]
        if custom:
            self.warn("networks", "networks %s: all services share one loopback network in the guest, so the "
                      "isolation between Compose networks is not modelled (every service can reach every other)"
                      % ", ".join(map(str, custom)))
        self.port_collisions()
        return self.emit()

    def find_jobs(self, order, raw):
        jobs = set()
        for n in order:
            dep = (raw[n] or {}).get("depends_on")
            if isinstance(dep, dict):
                for d, c in dep.items():
                    if isinstance(c, dict) and c.get("condition") == "service_completed_successfully":
                        jobs.add(d)
        return jobs

    def service(self, n, s, jobs):
        where = "service %s" % n
        hn = self.names[n]
        out = {"name": hn, "_compose": n, "_comments": []}
        for k in s:
            if k in HANDLED or k in IGNORED or str(k).startswith("x-"):
                continue
            if k in UNSUPPORTED:
                self.warn(k, "%s: %s: %s" % (where, k, UNSUPPORTED[k]))
            else:
                self.warn(k, "%s: %s: not understood by hydo-compose; ignored" % (where, k))
        if "build" in s:
            if "image" in s:
                self.warn("build", "%s: build: ignored, the image %s is used (build and push it, or `docker save` "
                          "your local build and point image: at the file)" % (where, s["image"]))
            else:
                raise ComposeError("%s: has build: and no image:. hydo needs an image file: build it, then "
                                   "`docker save -o %s.tar <tag>` and add `image: <tag>` to the Compose service "
                                   "(or set services[].image in hydo.yaml to the .tar)" % (where, n))
        if "image" not in s:
            raise ComposeError("%s: no image:" % where)
        ref = str(s["image"])
        info = self.image_info(ref)
        out["_info"] = info
        reg, repo, tag, digest = parse_ref(ref)
        stem = file_stem(reg, repo, tag, digest)
        idir = self.o.get("images_dir") or "images"
        plat = info.get("platforms") or {}
        if s.get("platform") and str(s["platform"]).split("/")[1:2] not in (["amd64"], ["arm64"]):
            self.warn("platform", "%s: platform %s: hydo runs linux/amd64 (x86_64) and linux/arm64 (Mac) only" %
                      (where, s["platform"]))
        img = {"x86_64": "%s/%s-amd64.docker.tar" % (idir, stem)}
        if "arm64" in plat or info.get("unresolved"):
            img["arm64"] = "%s/%s-arm64-oci" % (idir, stem)
        out["image"] = img if len(img) > 1 else img["x86_64"]
        out["image_ref"] = "%s:%s" % (info["name"], tag) if tag else info["name"]
        if info.get("unresolved"):
            out["_comments"].append("image %s: NOT resolved to a digest (--no-resolve); fetch-images.sh fetches the "
                                    "tag, which can change" % ref)
            self.warn("image", "%s: image %s is not pinned by digest (--no-resolve)" % (where, ref))
        else:
            out["_comments"].append("image %s = %s/%s@%s" % (ref, info["registry"], info["repo"], info["index_digest"]))
            for a in ("amd64", "arm64"):
                if a in plat:
                    out["_comments"].append("  linux/%s%s: %s" % (a, ("/" + plat[a]["variant"]) if plat[a]["variant"] else "",
                                                                    plat[a]["digest"]))
        cfg = (plat.get("amd64") or {}).get("config") or {}
        # CPU level: an amd64 image built only for x86-64-v3/v4, or a known one
        am = plat.get("amd64") or {}
        if am.get("variant") in ("v3", "v4"):
            self.cpu = "v3"
            out["_comments"].append("linux/amd64 variant %s: needs AVX2 (guest.cpu: v3)" % am["variant"])
            if am["variant"] == "v4":
                self.warn("cpu", "%s: the image is built for x86-64-v4 (AVX-512); hydo offers v3 at most" % where)
        base = repo.rsplit("/", 1)[-1]
        fam = next((f for f in FAMILIES if re.search(f[0], base)), None)
        if fam and fam[3] == "v3":
            self.cpu = "v3"
            out["_comments"].append("%s is known to need AVX2 (guest.cpu: v3)" % base)
        # command / entrypoint
        ent = words(s.get("entrypoint"), where + " entrypoint")
        cmd = words(s.get("command"), where + " command")
        if ent is not None:
            out["entrypoint"] = ent
        if cmd is not None:
            out["cmd"] = cmd
        if n in jobs:
            self.job(out, s, cfg, ent, cmd, where)
        # env: env_file first, environment overrides
        env = {}
        for ef in as_list(s.get("env_file")):
            path, req = (ef.get("path"), ef.get("required", True)) if isinstance(ef, dict) else (ef, True)
            p = path if os.path.isabs(path) else os.path.join(self.cdir, path)
            e, bare = parse_env_file(p, req)
            for k, v in e.items():
                env[k] = unesc(v)
            for k in bare:
                self.warn("env_file", "%s: %s in %s has no value; left out" % (where, k, path))
        for k, v in env_of(s.get("environment"), where, self.warn, self.interp, self.o.get("use_env")).items():
            env[k] = unesc(v)
        if env:
            out["env"] = env
        if s.get("working_dir"):
            out["workdir"] = str(s["working_dir"])
        if s.get("user") is not None:
            out["user"] = str(s["user"])
        user = out.get("user", cfg.get("User") or "")
        # ports
        ports = container_ports(s, where, self.warn)
        exposed = sorted(int(p.split("/")[0]) for p in (cfg.get("ExposedPorts") or {}) if p.split("/")[0].isdigit())
        if ports:
            out["port"] = ports[0]
            if len(set(ports)) > 1:
                out["_comments"].append("ports %s: network faults (N=/O=) act on the first, %d" % (sorted(set(ports)), ports[0]))
        elif exposed:
            out["port"] = exposed[0]
            out["_comments"].append("port from the image's EXPOSE %s" % exposed)
        out["_listen"] = sorted(set(ports) | set(exposed))
        out["addr"] = self.addr[n]
        if self.o.get("no_addr"):
            out["_noaddr"] = True
        # names
        aliases = [n]
        for k in ("container_name", "hostname"):
            if s.get(k):
                aliases.append(str(s[k]))
        nets = s.get("networks")
        if isinstance(nets, dict):
            for nn, nd in nets.items():
                if isinstance(nd, dict):
                    aliases += [str(a) for a in as_list(nd.get("aliases"))]
                    if nd.get("ipv4_address"):
                        self.warn("networks", "%s: ipv4_address %s ignored; the service is at %s" %
                                  (where, nd["ipv4_address"], self.addr[n]))
        self.hosts.append((self.addr[n], [a for i, a in enumerate(aliases) if a not in aliases[:i]]))
        for ln in as_list(s.get("links")):
            tgt, _, al = str(ln).partition(":")
            if al and tgt in self.addr:
                self.hosts.append((self.addr[tgt], [al]))
        eh = s.get("extra_hosts")
        for e in (eh.items() if isinstance(eh, dict) else [re.split(r"[:=]", str(x), 1) for x in as_list(eh)]):
            h, ip = e[0], (e[1] if len(e) > 1 else "")
            ip = "127.0.0.1" if ip in ("host-gateway", "") else ip
            self.hosts.append((ip, [str(h)]))
        # depends_on
        dep = s.get("depends_on")
        deps = []
        if isinstance(dep, list):
            deps = [str(d) for d in dep]
        elif isinstance(dep, dict):
            for d, c in dep.items():
                deps.append(str(d))
                if isinstance(c, dict):
                    cond = c.get("condition", "service_started")
                    if cond == "service_started":
                        out["_comments"].append("depends_on %s (service_started): hydo waits until %s is ready" % (d, d))
                    if c.get("required") is False:
                        self.warn("depends_on", "%s: depends_on %s required: false -- hydo always starts it" % (where, d))
        deps2 = []
        for d in deps:
            if d not in self.names:
                if d in (self.doc["services"] or {}):
                    self.warn("depends_on", "%s: depends on %s, which was left out (profiles)" % (where, d))
                    continue
                raise ComposeError("%s: depends_on names unknown service %s" % (where, d))
            deps2.append(self.names[d])
        if deps2:
            out["depends_on"] = deps2
        # healthcheck -> ready
        if n in jobs:
            out["ready"] = "true"
        else:
            self.readiness(out, s, cfg, user, where)
        # restart -> recovery
        rs = s.get("restart")
        if rs is not None and str(rs) in ("no", "False", "false") or rs is False:
            out["recovery"] = False
            out["_comments"].append('restart: "no" -> recovery: false (not started again after a power cut, as '
                                    'Docker does not restart it after a reboot; hydo still restarts it after a '
                                    'kill fault)')
        elif rs is not None and str(rs).startswith("on-failure"):
            out["_comments"].append("restart: %s: hydo restarts the service after every kill fault and after a power "
                                    "cut (crash recovery)" % rs)
        # volumes, secrets, tmpfs, writable /run + /dev/shm
        self.mounts(out, s, cfg, n, where)
        # RAM, data disk
        lim = mem_mib(s.get("mem_limit")) or mem_mib(((s.get("deploy") or {}).get("resources") or {})
                                                     .get("limits", {}).get("memory"))
        r = lim or (fam[1] if fam else DEFAULT_SVC_RAM)
        self.ram += r
        if any((m.get("src") or "").startswith("/data/") for m in out.get("mounts", [])):
            self.data_mib += fam[2] if fam else 128
        dp = s.get("deploy") or {}
        for k in dp:
            if k == "resources":
                continue
            if k == "replicas" and int(dp[k]) > 1:
                self.warn("deploy", "%s: deploy.replicas %s: one instance is started; copy the service for more" % (where, dp[k]))
            elif k not in ("replicas", "restart_policy", "labels"):
                self.warn("deploy", "%s: deploy.%s ignored (Swarm settings)" % (where, k))
        if (dp.get("resources") or {}).get("reservations", {}).get("devices"):
            self.warn("deploy", "%s: device reservations (GPUs) are not available" % where)
        return out

    def job(self, out, s, cfg, ent, cmd, where):
        """A one-shot service other services wait for (service_completed_successfully): it idles in
        the guest, and its command runs in the setup of each dependent (once per boot)."""
        e = ent if ent is not None else (cfg.get("Entrypoint") or [])
        c = cmd if cmd is not None else ([] if ent is not None else (cfg.get("Cmd") or []))
        argv = list(e) + list(c)
        if not argv:
            self.warn("job", "%s: is a one-shot job but its command is unknown (image not resolved); fill in "
                      "the job command in the dependents' setup:" % where)
        out["_job"] = argv
        out["entrypoint"] = ["/.hydo/busybox", "sleep", "2147483647"]
        out.pop("cmd", None)
        out["_comments"].append("a one-shot job (others depend on it with service_completed_successfully): it "
                                "idles here and its command runs in the setup: of each service that waits for it")

    def readiness(self, out, s, cfg, user, where):
        hc = s.get("healthcheck")
        src = "Compose healthcheck"
        if hc is None or (isinstance(hc, dict) and "test" not in hc and not hc.get("disable")):
            ih = cfg.get("Healthcheck")
            if ih and ih.get("Test"):
                hc = {"test": ih["Test"], "timeout": (ih.get("Timeout") or 0) // 1000000000 or None,
                      "interval": (ih.get("Interval") or 0) // 1000000000 or None,
                      "retries": ih.get("Retries"),
                      "start_period": (ih.get("StartPeriod") or 0) // 1000000000 or None}
                src = "the image's HEALTHCHECK"
        test = None
        if isinstance(hc, dict) and not hc.get("disable"):
            t = hc.get("test")
            if isinstance(t, list) and t:
                if t[0] == "NONE":
                    test = None
                elif t[0] == "CMD":
                    test = [unesc(str(x)) for x in t[1:]]
                elif t[0] == "CMD-SHELL":
                    test = ["/bin/sh", "-c", unesc(" ".join(str(x) for x in t[1:]))]
                else:
                    test = [unesc(str(x)) for x in t]
            elif isinstance(t, str) and t:
                test = ["/bin/sh", "-c", unesc(t)]
        hn = out["name"]
        if test:
            to = duration_s(hc.get("timeout"), 30)
            pre = ["/.hydo/busybox", "chpst", "-u", user] if user and user not in ("root", "0", "0:0") else []
            out["ready"] = "timeout %d hydo-oci %s %s" % (min(to, 30), hn, " ".join(shlex.quote(x) for x in pre + test))
            budget = duration_s(hc.get("start_period"), 0) + duration_s(hc.get("interval"), 30) * int(hc.get("retries") or 3)
            out["ready_timeout"] = max(120, min(600, budget + 60))
            out["_comments"].append("ready: from %s" % src)
        elif out.get("port"):
            out["ready"] = "nc -z -w 2 %s %d" % (out["addr"], out["port"])
            out["_comments"].append("ready: no healthcheck; a TCP connect to its port (TODO: a real readiness "
                                    "check, e.g. a query, if the port opens before the service can serve)")
        else:
            out["ready"] = "true"
            out["_comments"].append("ready: TODO no healthcheck and no port -- `true` means started")
            if not out.get("_job"):
                self.warn("healthcheck", "%s: no healthcheck and no port: readiness is just `started`; write a "
                          "ready: check" % where)

    def mounts(self, out, s, cfg, n, where):
        mts = [{"src": "/etc/hosts", "dst": "/etc/hosts"},
               {"tmpfs": True, "dst": "/run", "copy_up": True},
               {"tmpfs": True, "dst": "/dev/shm", "size": "64m"}]
        seen = {"/etc/hosts", "/run", "/dev/shm"}

        def add(m, what):
            d = posixpath.normpath(m["dst"])
            if not PATH_OK.match(d):
                self.warn("volumes", "%s: %s target %r has characters hydo cannot mount (spaces, quotes); skipped"
                          % (where, what, d))
                return
            if d in ("/", "/proc", "/sys", "/dev", "/tmp", "/data"):
                if d == "/data":
                    self.warn("volumes", "%s: %s on /data: the image's /data is already the data disk's root "
                              "(shared by every service); the mount was not added" % (where, what))
                elif d == "/tmp":
                    self.notes.append("%s: %s on /tmp: /tmp is shared with the guest (and every service)" % (where, what))
                else:
                    self.warn("volumes", "%s: %s on %s cannot be mounted; skipped" % (where, what, d))
                return
            if d in seen:
                mts[:] = [x for x in mts if x["dst"] != d]
            seen.add(d)
            m["dst"] = d
            mts.append(m)

        for v in as_list(s.get("volumes")):
            self.volume(v, add, n, where)
        for v in as_list(s.get("tmpfs")):
            p, _, opt = str(v).partition(":")
            mo = re.search(r"size=(\d+[kmg]?)", opt)
            add(dict({"tmpfs": True, "dst": p}, **({"size": mo.group(1)} if mo else {})), "tmpfs")
        for sec in as_list(s.get("secrets")):
            name, tgt = (sec.get("source"), sec.get("target")) if isinstance(sec, dict) else (str(sec), None)
            d = (self.doc.get("secrets") or {}).get(name) or {}
            if not d.get("file"):
                self.warn("secrets", "%s: secret %s has no file: (environment/external secrets are not supported); "
                          "skipped" % (where, name))
                continue
            src = d["file"] if os.path.isabs(d["file"]) else os.path.join(self.cdir, d["file"])
            tgt = tgt if tgt and tgt.startswith("/") else "/run/secrets/%s" % (tgt or name)
            gp = self.copy_bind(src, where)
            if gp:
                add({"src": gp, "dst": tgt}, "secret %s" % name)
        covered = {m["dst"] for m in mts}
        for v in sorted((cfg.get("Volumes") or {})):
            vv = posixpath.normpath(v)
            if vv in covered or any(vv.startswith(c + "/") for c in covered if c != "/"):
                continue
            if vv == "/data":
                continue                     # the image's /data is the data disk anyway
            k = "anon-%s-%s" % (n, vv.strip("/").replace("/", "-"))
            self.data_dirs.append("vol/" + k)
            add({"src": "/data/vol/" + k, "dst": vv, "copy_up": True}, "image VOLUME")
            out["_comments"].append("the image declares VOLUME %s: an anonymous volume on the data disk" % vv)
        out["mounts"] = mts

    def copy_bind(self, src, where):
        if not os.path.exists(src):
            return None
        for sp in SPECIAL_HOST:
            if src == sp or src.startswith(sp + "/"):
                return None
        for s0, g0 in self.bind_files:      # one copy per source, shared by the services
            if s0 == src:
                return g0
        k = len(self.bind_files)
        base = re.sub(r"[^A-Za-z0-9._-]", "_", os.path.basename(src.rstrip("/")) or "root")
        gp = "/etc/hydo/compose/bind/%d-%s" % (k, base)
        self.bind_files.append((src, gp))
        return gp

    def volume(self, v, add, n, where):
        ro = False
        if isinstance(v, dict):
            typ, src, tgt = v.get("type", "volume"), v.get("source"), v.get("target")
            ro = bool(v.get("read_only"))
            if typ == "tmpfs":
                sz = (v.get("tmpfs") or {}).get("size")
                add(dict({"tmpfs": True, "dst": tgt}, **({"size": str(sz)} if sz else {})), "tmpfs")
                return
            if typ not in ("volume", "bind"):
                self.warn("volumes", "%s: volume type %s not supported; skipped" % (where, typ))
                return
        else:
            parts = str(v).split(":")
            if len(parts) == 1:
                typ, src, tgt = "volume", None, parts[0]
            else:
                src, tgt = parts[0], parts[1]
                ro = len(parts) > 2 and "ro" in parts[2].split(",")
                typ = "bind" if src.startswith((".", "/", "~")) else "volume"
        if not tgt or not str(tgt).startswith("/"):
            self.warn("volumes", "%s: volume %r: no absolute target; skipped" % (where, v))
            return
        if typ == "volume":
            if src:
                if src not in (self.doc.get("volumes") or {}):
                    self.warn("volumes", "%s: volume %s is not declared under the top-level volumes:" % (where, src))
                k = re.sub(r"[^A-Za-z0-9._-]", "_", src)
            else:
                k = "anon-%s-%s" % (n, str(tgt).strip("/").replace("/", "-"))
            self.data_dirs.append("vol/" + k)
            if ro:
                self.notes.append("%s: volume %s is mounted read-write in the guest (:ro is not enforced)" % (where, src))
            add({"src": "/data/vol/" + k, "dst": tgt, "copy_up": True}, "volume %s" % (src or "(anonymous)"))
            return
        p = os.path.expanduser(src)
        if not os.path.isabs(p):
            p = os.path.normpath(os.path.join(self.cdir, p))
        for sp in SPECIAL_HOST:
            if p == sp or p.startswith(sp + "/"):
                self.warn("volumes", "%s: bind mount of the host's %s skipped: the guest is a separate machine%s" %
                          (where, sp, " (the guest's clock is UTC)" if "time" in sp else
                           " (there is no Docker daemon inside)" if "docker" in sp else ""))
                return
        if not os.path.exists(p):
            k = "bind-%s" % re.sub(r"[^A-Za-z0-9._-]", "_", os.path.relpath(p, self.cdir).strip("./") or n)
            self.data_dirs.append("vol/" + k)
            self.warn("volumes", "%s: bind mount source %s does not exist (Docker would create an empty directory): "
                      "it is an empty directory on the data disk, /data/vol/%s" % (where, src, k))
            add({"src": "/data/vol/" + k, "dst": tgt}, "bind %s" % src)
            return
        if os.path.isdir(p) and not ro:
            self.warn("volumes", "%s: bind mount %s:%s is copied into the READ-ONLY root at build time; if the program "
                      "writes there, make it a named volume (then it lives on the data disk)" % (where, src, tgt))
        gp = self.copy_bind(p, where)
        add({"src": gp, "dst": tgt}, "bind %s" % src)

    def port_collisions(self):
        by = {}
        for s in self.svcs:
            for p in s.get("_listen") or []:
                by.setdefault(p, []).append(s["name"])
        for p, ss in sorted(by.items()):
            if len(ss) > 1:
                self.warn("ports", "services %s all use container port %d. In Compose each has its own network; in "
                          "the guest they share one, so if they listen on 0.0.0.0:%d only the first starts. Make "
                          "each listen on its own addr: (%s) or move one to another port, e.g. with env:/cmd:" %
                          (", ".join(ss), p, p, ", ".join("%s=%s" % (x, next(y["addr"] for y in self.svcs if y["name"] == x)) for x in ss)))

    # -------------------------------------------------------------- output
    def emit(self):
        name = self.o.get("name") or self.doc.get("name") or os.path.basename(self.cdir) or "compose"
        name = re.sub(r"[^a-z0-9-]", "-", str(name).lower()).strip("-")
        if not name or not name[0].isalpha():
            name = "c-" + name
        ram = max(1024, (self.ram + 255) // 256 * 256)
        if ram > 3072:
            self.warn("ram", "the services need about %d MiB; hydo's x86_64 guest has at most 3072 MiB "
                      "(guest.ram_mib was set to 3072)" % ram)
            ram = 3072
        data_mib = max(512, (self.data_mib + 255) // 256 * 256)
        # job commands go into the dependents' setup
        jobs = {s["name"]: s for s in self.svcs if "_job" in s}
        for s in self.svcs:
            pre = []
            for d in s.get("depends_on") or []:
                if d in jobs:
                    j = jobs[d]
                    u = (j.get("user") or ((j["_info"].get("platforms") or {}).get("amd64") or {})
                         .get("config", {}).get("User") or "")
                    chp = ["/.hydo/busybox", "chpst", "-u", u] if u and u not in ("root", "0", "0:0") else []
                    argv = chp + (j["_job"] or ["TODO-job-command"])
                    pre.append("[ -f /tmp/hydo/job.%s.done ] || { hydo-oci %s %s && : > /tmp/hydo/job.%s.done; }"
                               % (d, d, " ".join(shlex.quote(x) for x in argv), d))
            if pre:
                s["setup"] = " && ".join(pre)
        L = []
        L.append(comment(
            "Generated by hvx/onboard/hydo-compose from %s.\n"
            "A STARTING POINT: the services, images, readiness checks and volumes come from the Compose\n"
            "file; the workload and the invariants are TODO (search for TODO). The hydo-onboard skill\n"
            "(`hydo guide`) fills them in. MANIFEST.md, \"From Docker Compose\", explains\n"
            "every mapping.\n"
            "\n"
            "1. sh %s           # fetch the images by digest (needs network, once)\n"
            "2. write the workload and the invariants below\n"
            "3. HYDO_SEEDRNG=<pinned hvx_seedrng> hvx/onboard/hydo-build %s   (or: hydo build)"
            % (os.path.relpath(self.cpath, self.odir), "fetch-images.sh", os.path.basename(self.out))))
        if self.warnings:
            L.append("#\n" + comment("WARNINGS (also printed by hydo-compose):\n" +
                                     "\n".join("- " + t for _, t in self.warnings)))
        if self.notes:
            L.append("#\n" + comment("Notes:\n" + "\n".join("- " + t for t in self.notes)))
        L.append("name: %s\n" % name)
        L.append("description: %s\n" % q("imported from %s (%d services); TODO: what is tested" %
                                         (os.path.basename(self.cpath), len(self.svcs))))
        L.append("\nguest:\n")
        L.append("  ram_mib: %d   # a guess: %d MiB for the services and the kernel; raise it if a service runs out\n"
                 % (ram, self.ram))
        if self.cpu:
            L.append("  cpu: v3       # an image needs AVX/AVX2 (see the service's comment)\n")
        else:
            L.append("  # cpu: v3     # uncomment if a service stops with \"invalid opcode\" (an image built for AVX2)\n")
        L.append("\ndata:\n  size_mib: %d   # the data disk: every volume lives here (the fault target)\n" % data_mib)
        dd = []
        for d in self.data_dirs:
            if d not in dd:
                dd.append(d)
        if dd:
            L.append("  dirs: %s\n" % flow_list(dd))
        L.append("\nroot:\n")
        if self.bind_files:
            L.append("  files:                 # bind-mount sources, copied into the read-only root\n")
            for src, gp in self.bind_files:
                rs = os.path.relpath(src, self.odir)
                L.append("    - %s\n" % flow_map({"src": rs, "dst": gp}))
        hosts = "127.0.0.1 localhost\n::1 localhost ip6-localhost ip6-loopback\n"
        for ip, al in self.hosts:
            hosts += "%s %s\n" % (ip, " ".join(al))
        L.append("  text:\n    /etc/hosts: " + block(hosts, 6).replace("|\n", "|   # Compose service names -> each service's addr:\n", 1))
        L.append("\nservices:\n")
        for s in self.svcs:
            L.append(comment("Compose service %s" % s["_compose"], 2))
            for c in s["_comments"]:
                L.append(comment(c, 2))
            L.append("  - name: %s\n" % s["name"])
            if isinstance(s["image"], dict):
                L.append("    image: %s\n" % flow_map(s["image"]))
            else:
                L.append("    image: %s\n" % q(s["image"]))
            L.append("    image_ref: %s\n" % q(s["image_ref"]))
            for k in ("entrypoint", "cmd"):
                if k in s:
                    L.append("    %s: %s\n" % (k, flow_list(s[k])))
            if s.get("env"):
                L.append("    env:\n")
                for k, v in s["env"].items():
                    L.append("      %s: %s\n" % (k, json.dumps(v)))
            for k in ("workdir", "user"):
                if k in s:
                    L.append("    %s: %s\n" % (k, json.dumps(s[k])))
            if "port" in s:
                L.append("    port: %d\n" % s["port"])
            if not s.get("_noaddr"):
                L.append("    addr: %s\n" % s["addr"])
            if s.get("depends_on"):
                L.append("    depends_on: %s\n" % flow_list(s["depends_on"]))
            if s.get("setup"):
                L.append("    setup: %s\n" % json.dumps(s["setup"]))
            L.append("    ready: %s\n" % json.dumps(s["ready"]))
            if s.get("ready_timeout"):
                L.append("    ready_timeout: %d\n" % s["ready_timeout"])
            if s.get("recovery") is False:
                L.append("    recovery: false\n")
            L.append("    mounts:\n")
            for m in s["mounts"]:
                d = {}
                for k in ("src", "tmpfs", "dst", "copy_up", "size"):
                    if k in m and m[k] not in (None, False):
                        d[k] = m[k]
                L.append("      - %s\n" % flow_map(d))
        first = self.svcs[0]
        ex = next((s for s in self.svcs if s.get("port")), first)
        L.append("\n")
        L.append(comment(
            "TODO(workload): one step = one client operation against the system (an HTTP request, a\n"
            "query), appended to $HYDO_ACKED only when the system acknowledged it. Faults are placed\n"
            "at step numbers. Each Compose service name resolves (e.g. %s -> %s); run tools inside\n"
            "an image with `hydo-oci <service> <command>`. Ask the hydo-onboard skill to write it." %
            (ex["_compose"], ex["addr"])))
        L.append("workload:\n  steps: 20\n  run: " + block(
            "i=$HYDO_FROM\n"
            "while [ $i -lt $HYDO_TO ]; do\n"
            "  # TODO: one operation, e.g.\n"
            "  #   wget -q -T 5 -O /dev/null --post-data \"id=$i\" http://%s:%s/ && echo $i >> $HYDO_ACKED\n"
            "  echo \"hydo-compose: TODO write the workload (hydo.yaml)\" > /dev/console; exit 1\n"
            "  i=$((i + 1))\n"
            "done" % (ex["_compose"], ex.get("port", 80)), 4))
        L.append("  timeout: 300\n\n")
        L.append(comment(
            "TODO(invariants): shell commands, exit 0 = holds, 1 = violated, 2 = cannot check now.\n"
            "Typical: every id in $HYDO_ACKED can be read back after the run (and after a power cut);\n"
            "type: eventually for \"processed within N s\". `hydo guide invariant-patterns`"))
        L.append("invariants:\n  - id: todo.acked_operations_survive\n"
                 "    msg: TODO every acknowledged operation is still there at the end\n    cmd: " +
                 block("echo \"TODO: write the invariants (hydo.yaml)\"; exit 2", 6))
        L.append("\nfaults: [kill, pause, network, oneway, cut, clock, disk]\n")
        return "".join(L)

    def fetch_script(self):
        L = ["#!/bin/sh",
             "# Fetch the images of %s by digest into %s/ (generated by hvx/onboard/hydo-compose;"
             % (os.path.basename(self.cpath), self.o.get("images_dir") or "images"),
             "# hydo-build never pulls). Needs skopeo (or nix: nix build nixpkgs#skopeo).",
             "set -eu", 'cd "$(dirname "$0")"',
             "nix_skopeo() {",
             "  for p in $(nix build --no-link --print-out-paths nixpkgs#skopeo); do",
             '    [ -x "$p/bin/skopeo" ] && { echo "$p/bin/skopeo"; return; }',
             "  done",
             '  echo "fetch-images.sh: no skopeo" >&2; exit 1', "}",
             'SKOPEO=${SKOPEO:-$(command -v skopeo || nix_skopeo)}',
             'POL=$(mktemp); trap \'rm -f "$POL"\' EXIT',
             'echo \'{"default":[{"type":"insecureAcceptAnything"}]}\' > "$POL"   # the digests pin the content',
             "mkdir -p %s" % shlex.quote(self.o.get("images_dir") or "images")]
        done = set()
        for s in self.svcs:
            info = s["_info"]
            img = s["image"] if isinstance(s["image"], dict) else {"x86_64": s["image"]}
            plat = info.get("platforms") or {}
            for arch, path in sorted(img.items()):
                if path in done:
                    continue
                done.add(path)
                a = {"x86_64": "amd64", "arm64": "arm64"}[arch]
                if a in plat and plat[a].get("digest"):
                    src = "docker://%s/%s@%s" % (info["registry"], info["repo"], plat[a]["digest"])
                else:
                    src = "docker://%s/%s:%s" % (info["registry"], info["repo"], info.get("tag") or "latest")
                    L.append("# NOT pinned: %s" % src)
                dst = ("docker-archive:%s:%s" if arch == "x86_64" else "oci:%s:%s") % (path, s["image_ref"])
                test = "-f" if arch == "x86_64" else "-d"
                L.append('[ %s %s ] || "$SKOPEO" --policy "$POL" copy %s %s' % (
                    test, shlex.quote(path), shlex.quote(src), shlex.quote(dst)))
        L.append('ls -ld %s' % " ".join(shlex.quote(p) for p in sorted(done)))
        return "\n".join(L) + "\n"


def main(argv=None):
    a = list(sys.argv[1:] if argv is None else argv)
    if not a or a[0] in ("-h", "--help"):
        print(__doc__)
        return 0 if a else 2
    opts = {"env_files": [], "profiles": []}
    out = None
    path = None
    i = 0
    while i < len(a):
        x = a[i]
        if x in ("--out", "-o", "--name", "--images-dir", "--lock", "--env-file", "--profile"):
            if i + 1 >= len(a):
                print("hydo-compose: %s needs a value" % x, file=sys.stderr); return 2
            v = a[i + 1]
            if x in ("--out", "-o"):
                out = v
            elif x == "--env-file":
                opts["env_files"].append(v)
            elif x == "--profile":
                opts["profiles"].append(v)
            else:
                opts[x[2:].replace("-", "_")] = v
            i += 2
        elif x == "--use-env":
            opts["use_env"] = True; i += 1
        elif x == "--no-resolve":
            opts["no_resolve"] = True; i += 1
        elif x == "--no-addr":
            opts["no_addr"] = True; i += 1
        elif x.startswith("-"):
            print("hydo-compose: unknown option %s" % x, file=sys.stderr); return 2
        elif path is None:
            path = x; i += 1
        else:
            print("hydo-compose: one Compose file only", file=sys.stderr); return 2
    if path is None:
        print(__doc__); return 2
    out = out or os.path.join(os.path.dirname(os.path.abspath(path)), "hydo.yaml")
    try:
        imp = Importer(path, out, opts)
        text = imp.run()
        hb = _hydo_build()
        m = hb.parse_yaml(text, "hydo.yaml")      # the output must be what hydo-build reads
        hb.validate(m)
        hb.service_order(m["services"])
    except (ComposeError, OSError) as e:
        print("hydo-compose: %s" % e, file=sys.stderr)
        return 1
    except Exception as e:  # a generated manifest hydo-build would refuse is a bug here
        if type(e).__name__ in ("YamlError", "ManifestError"):
            print("hydo-compose: internal error, the generated manifest is not valid: %s" % e, file=sys.stderr)
            return 3
        raise
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    with open(out, "w") as f:
        f.write(text)
    fs = os.path.join(os.path.dirname(os.path.abspath(out)), "fetch-images.sh")
    with open(fs, "w") as f:
        f.write(imp.fetch_script())
    os.chmod(fs, 0o755)
    if imp.lock_dirty:
        with open(imp.lock_path, "w") as f:
            json.dump({"generated_by": "hvx/onboard/hydo-compose", "images": imp.lock}, f, indent=1, sort_keys=True)
            f.write("\n")
    for _, t in imp.warnings:
        print("hydo-compose: warning: %s" % t, file=sys.stderr)
    print("hydo-compose: wrote %s (%d services, %d warnings), %s%s" % (
        out, len(imp.svcs), len(imp.warnings), fs, ", %s" % imp.lock_path if imp.lock_dirty else ""), file=sys.stderr)
    print("hydo-compose: next: sh %s; then write the workload and the invariants (TODO in %s)" % (fs, out),
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
