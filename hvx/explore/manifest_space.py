"""manifest_space -- the explorer's fault space for an onboarded project (hydo.yaml, see
hvx/onboard/MANIFEST.md), instead of the four-service stack's fixed names.

explore.py --manifest <hydo.yaml> (or an explore-ob-* image, whose manifest is read from the
image itself) calls apply(), which rebinds the few module globals the generators, sweeps and
the shrinker draw from:

    SVCS        services that kill (K=) faults pick from        stack: mysql kafka redis
    NET_SVCS    services with a port, for N= / O=               stack: mysql kafka redis
                (plus, when two or more services have an addr:, the links "a-b" and "a-*"
                between them -- hydo-init-links.sh; no manifest without addr: changes)
    PAUSE_SVCS  services that pause (P=) faults pick from       stack: mysql kafka redis front
    NORDERS     the workload's step count                       stack: 30
    KNOBS       (token, relaxed value) pairs a knob draw picks  stack: M=2 M=0 R=everysec R=no
    KNOB_TOKENS knob tokens render / parse / shrink handle      stack: M R
    FC_PAIRS    (service, peer) pairs a freeze-at-a-call (F=,    stack: none
                --freeze-call) picks from: only when faults: lists freeze

and installs SPACE, whose restrict() render() applies last: it drops the fault kinds the
manifest's `faults:` list leaves out (a lying disk falls back to a plain power cut when only
`cut` is allowed) and network faults on services without a port. Without --manifest none of
this runs and explore.py is byte-for-byte the stack explorer.

The draws themselves are unchanged (same SplitMix streams, same ranges relative to NORDERS),
so a manifest with the stack's names and step count yields the stack explorer's schedules.
"""
import importlib.machinery
import importlib.util
import json
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
HYDO_BUILD = os.path.join(os.path.dirname(HERE), "onboard", "hydo-build")
ROOT_OFFSET = 2048 * 512            # hydo-build puts the root filesystem at LBA 2048
FAULT_KINDS = ["kill", "pause", "network", "oneway", "cut", "clock", "disk"]
# guest schedule tokens with a fixed meaning (faults; Q/X = concurrency and timed kill, hydo-c7's branch);
# a manifest knob may not reuse one, or parse() would read the knob as that fault. M/R stay knob names.
RESERVED_TOKENS = {"K", "P", "N", "O", "C", "T", "Q", "X", "B", "F", "W"}   # W: the test composer


def _parser():
    """hydo-build's own YAML-subset parser and validator (read-only use)."""
    loader = importlib.machinery.SourceFileLoader("hydo_build", HYDO_BUILD)
    spec = importlib.util.spec_from_loader("hydo_build", loader)
    mod = importlib.util.module_from_spec(spec)
    loader.exec_module(mod)
    return mod


def read_text(path):
    return open(path).read()


def _composer():
    import sys
    d = os.path.join(os.path.dirname(HERE), "onboard")
    if d not in sys.path:
        sys.path.insert(0, d)
    import hydo_composer
    return hydo_composer


def embedded_compose(image_path):
    """The test composer's plan an image carries (/etc/hydo/compose.json), or None."""
    p = subprocess.run(["debugfs", "-R", "cat /etc/hydo/compose.json", "%s?offset=%d" % (image_path, ROOT_OFFSET)],
                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    if p.returncode != 0 or not p.stdout.strip().startswith("{"):
        return None
    return json.loads(p.stdout)


def embedded_text(image_path):
    """The hydo.yaml an explore-ob-* image carries in /etc/hydo/ (debugfs, no mount, no root)."""
    p = subprocess.run(["debugfs", "-R", "cat /etc/hydo/hydo.yaml", "%s?offset=%d" % (image_path, ROOT_OFFSET)],
                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    if p.returncode != 0 or not p.stdout.strip():
        raise SystemExit("manifest_space: no /etc/hydo/hydo.yaml in %s (not a hydo-build image?)" % image_path)
    return p.stdout


class Space:
    def __init__(self, text, name="hydo.yaml", compose=None, folder_dir=None):
        """compose: the plan meta an image carries (embedded_compose); folder_dir: the directory of
        a hydo.yaml given by path, whose hydo/ folder (workload/, checks/) is read the way
        hydo-build reads it. Either way the test composer's rewrite runs before validate."""
        hb = _parser()
        m = json.loads(text) if name.endswith(".json") else hb.parse_yaml(text, name)
        self.compose = None
        if compose is not None or folder_dir is not None or "test" in m:
            hc = _composer()
            if compose is not None:
                m["test"] = hc.test_from_meta(compose)
                folder = None
            else:
                folder = hc.scan_folder(folder_dir, hb.ManifestError) if folder_dir else None
            self.compose = hc.expand(m, hb.ManifestError, folder)
        hb.validate(m)
        self.m = m
        self.name = m.get("name", "?")
        svcs = [s["name"] for s in m["services"]]
        self.ported = [s["name"] for s in m["services"] if s.get("port")]
        addr = [s["name"] for s in m["services"] if s.get("addr")]
        # link faults (MANIFEST.md "Link faults"): every pair, then each service against the rest
        self.links = ([] if len(addr) < 2 else
                      ["%s-%s" % (a, b) for i, a in enumerate(addr) for b in addr[i + 1:]] +
                      (["%s-*" % a for a in addr] if len(addr) > 2 else []))
        self.svcs = svcs
        self.fc_pairs = self._fc_pairs(m["services"])
        self.steps = int(m["workload"]["steps"])
        if self.steps < 4:
            raise SystemExit("manifest_space: workload.steps=%d; the generators need at least 4 steps" % self.steps)
        self.faults = list(m.get("faults") or FAULT_KINDS)
        self.knobs = []                                 # (token, safe value, [relaxed values])
        for k in m.get("knobs") or []:
            safe = str(k.get("safe", k["default"]))
            vals = [str(v) for v in (k.get("values") or []) if str(v) != safe]
            if k["token"] in RESERVED_TOKENS:
                raise SystemExit("manifest_space: knob token %r collides with a built-in schedule token "
                                 "(reserved: %s); rename the knob" % (k["token"], " ".join(sorted(RESERVED_TOKENS))))
            self.knobs.append((k["token"], safe, vals))

    @staticmethod
    def _fc_pairs(services):
        """(svc, peer) pairs for F=: peer listens (has a port:) and is in svc's depends_on closure,
        i.e. svc calls it, directly or through the services it depends on. A manifest whose
        depends_on names no such peer for any service: every ordered pair with a listening peer.
        (The init tells the pair's packets apart by both addr:, else by the peer's port.)"""
        by = {s["name"]: s for s in services}
        ok = lambda a, b: a != b and bool(by[b].get("port"))
        def closure(a):
            seen, todo = [], list(by[a].get("depends_on") or [])
            while todo:
                d = todo.pop(0)
                if d in by and d not in seen:
                    seen.append(d)
                    todo += list(by[d].get("depends_on") or [])
            return seen
        names = [s["name"] for s in services]
        pairs = [(a, b) for a in names for b in closure(a) if ok(a, b)]
        pairs = pairs or [(a, b) for a in names for b in names if ok(a, b)]
        # no service calls another (a single server): freeze a server in its conversation with
        # the workload's client ("workload" peer, hydo-init-freeze.sh)
        return pairs or [(a, "workload") for a in names if by[a].get("port")]

    def fc_match(self):
        """freeze_match: as (string, direction) pairs: "<" = in packets the frozen service
        receives (replies:), ">" = in packets it sends (requests:), None = a plain list (either)."""
        fm = self.m.get("freeze_match") or []
        if isinstance(fm, dict):
            return [(x, "<") for x in fm.get("replies") or []] + [(x, ">") for x in fm.get("requests") or []]
        return [(x, None) for x in fm]

    def describe(self):
        d = {"project": self.name, "services": self.svcs, "network": self.ported + self.links, "steps": self.steps,
             "faults": self.faults, "knobs": {t: {"safe": s, "relaxed": v} for t, s, v in self.knobs}}
        if self.composed():
            d["composer"] = {"commands": [c["id"] for c in self.compose["commands"]],
                             "checks": [c["name"] for c in self.compose["checks"]], "params": self.compose["params"]}
        if "freeze" in self.faults:
            d["freeze_pairs"] = ["%s>%s" % p for p in self.fc_pairs]
            d["freeze_match"] = ["%s%s" % ({"<": "reply:", ">": "request:"}.get(dr, ""), x) for x, dr in self.fc_match()]
        return d

    def composed(self):
        """True when the workload is the test composer's (the explorer then draws W=)."""
        return bool(self.compose and self.compose.get("composed"))

    def apply(self, ex):
        """Rebind explore.py's generator globals (ex = the explore module's globals())."""
        ex["SVCS"] = list(self.svcs) or ["-"]     # "-": a project without services; restrict() drops it
        ex["NET_SVCS"] = list(self.ported) + list(self.links) or list(self.svcs) or ["-"]   # placeholder; restrict() drops them
        ex["PAUSE_SVCS"] = list(self.svcs) or ["-"]
        ex["NORDERS"] = self.steps
        pairs = [(t, v) for t, _, vals in self.knobs for v in vals]
        ex["KNOBS"] = pairs or [("-", "")]                        # placeholder; restrict() drops it
        ex["KNOB_TOKENS"] = [t for t, _, _ in self.knobs]
        if "freeze" in self.faults:     # only then: the explorer of every other manifest is unchanged
            ex["FC_PAIRS"] = list(self.fc_pairs)
            ex["FC_MATCH"] = self.fc_match()
        ex["SPACE"] = self

    def restrict(self, s):
        """Drop what this project does not allow. Idempotent; never adds anything."""
        s = dict(s)
        f = set(self.faults)
        for kind, key in (("kill", "K"), ("pause", "P"), ("network", "N"), ("oneway", "O"), ("clock", "T")):
            if kind not in f:
                s.pop(key, None)
        for key in ("N", "O"):
            if key in s and s[key][0] not in self.ported and s[key][0] not in self.links:
                s.pop(key)
        if "F" in s and ("freeze" not in f or s["F"][0] not in self.svcs or s["F"][2] not in self.svcs + ["workload"]):
            s.pop("F")
        for key in ("K", "P", "X"):
            if key in s and s[key][0] == "-":    # placeholder of a project without services
                s.pop(key)
        if s["disk"].startswith(("lying", "ioerr")) and "disk" not in f:
            s["disk"] = "cut" if (s["disk"].startswith("lying") and "cut" in f) else "none"
        if s["disk"] == "cut" and "cut" not in f:
            s["disk"] = "none"
        if s["disk"] == "none" or s["disk"].startswith("ioerr"):
            s.pop("C", None)
        tokens = {t for t, _, _ in self.knobs}
        for key in list(s):
            if len(key) == 1 and key.isupper() and key not in "KNCPTOF" and key not in tokens and \
                    not (key == "B" and self.m.get("buggify") is True) and \
                    not (key == "W" and self.composed()):   # buggify: true keeps B= (the BUGGIFY seed); W: the composer
                s.pop(key)
        s.pop("-", None)
        return s

    SWEEP_KINDS = {"kill": ["kill"], "cut": ["cut"], "net": ["network"], "killcut": ["kill", "cut"],
                   "pause": ["pause"], "clock": ["clock"], "oneway": ["oneway"], "freeze": ["freeze"]}

    def check_sweep(self, name):
        need = [k for k in self.SWEEP_KINDS.get(name, []) if k not in self.faults]
        if need or (name in ("net", "oneway") and not (self.ported or self.links)):
            raise SystemExit("manifest_space: sweep %r needs fault kinds %s and a service with a port; "
                             "%s allows %s" % (name, self.SWEEP_KINDS.get(name), self.name, self.faults))

    def unsafe_knob(self, s):
        return any(t in s and str(s[t]) != safe for t, safe, _ in self.knobs)


def load(manifest=None, image_path=None):
    if manifest:
        return Space(read_text(manifest), os.path.basename(manifest),
                     folder_dir=os.path.dirname(os.path.abspath(manifest)))
    return Space(embedded_text(image_path), "hydo.yaml", compose=embedded_compose(image_path))


if __name__ == "__main__":          # python3 manifest_space.py <hydo.yaml | image>
    import sys
    p = sys.argv[1]
    sp = load(image_path=p) if p.endswith(".img") else load(manifest=p)
    print(json.dumps(sp.describe(), indent=1))
