# Identity check for network: bridge (hydo-init-netns.sh, Milestone 95). usage:
#   python3 netns_ident.py <reference tree> [<new tree>]       (trees = repository roots; new: this one)
# For every manifest of the reference tree (hvx/onboard/examples/**, hvx-mac/explore/examples/**, the
# importer's test fixtures are Compose files and not manifests), none of which has network: bridge:
# (1) hydo-build: the compiled /etc/hydo/manifest.sh and prelude.sh, /sbin/init, /etc/hydo/world.json
#     and the default /etc/hosts of the new hydo-build equal the reference's byte for byte, and so do
#     the staged tools and kernel modules it decides on; hydo_oci's hydo-oci wrapper is unchanged;
# (2) manifest_space: describe() (services, network targets incl. links, freeze pairs, protocol
#     targets, knobs) is identical;
# (3) explore.py: with each manifest applied, every generator composed as explore.py main does, with
#     the optional random streams on and off, gives byte-identical schedule strings, finding classes
#     and shrink candidates.
# Then, on the manifests WITH network: bridge in the new tree: links are only the pairs that talk.
import glob, importlib.machinery, importlib.util, itertools, json, os, sys

NEW = os.path.abspath(sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(__file__), "..", ".."))
REF = os.path.abspath(sys.argv[1])


def load(path, name):
    if path.endswith(".py"):
        spec = importlib.util.spec_from_file_location(name, path)
    else:
        spec = importlib.util.spec_from_loader(name, importlib.machinery.SourceFileLoader(name, path))
    m = importlib.util.module_from_spec(spec)
    sys.path.insert(0, os.path.dirname(path))
    try:
        spec.loader.exec_module(m)
    finally:
        sys.path.pop(0)
    return m


def tree(root, tag):
    hb = load(os.path.join(root, "hvx/onboard/hydo-build"), "hb_" + tag)
    oc = load(os.path.join(root, "hvx/onboard/hydo_oci.py"), "oci_" + tag)
    ex = load(os.path.join(root, "hvx/explore/explore.py"), "ex_" + tag)
    ms = load(os.path.join(root, "hvx/explore/manifest_space.py"), "ms_" + tag)
    return hb, oc, ex, ms


HB_A, OCI_A, EX_A, MS_A = tree(REF, "ref")
HB_B, OCI_B, EX_B, MS_B = tree(NEW, "new")
assert OCI_A.HYDO_OCI == OCI_B.HYDO_OCI, "hydo-oci wrapper changed"


def manifests(root):
    out = []
    for pat in ("hvx/onboard/examples/**/*.yaml", "hvx/onboard/examples/**/*.json",
                "hvx-mac/explore/examples/**/*.yaml", "hvx-mac/explore/examples/**/*.json"):
        out += glob.glob(os.path.join(root, pat), recursive=True)
    keep = []
    for p in sorted(set(out)):
        b = os.path.basename(p)
        if b in ("docker-compose.yml", "docker-compose.yaml", "hydo-compose.lock.json") or "/images/" in p \
                or b.endswith(".lock.json") or "/hydo/checks/" in p or "/hydo/workload/" in p:
            continue
        keep.append(p)
    return keep


def compiled(hb, path, new):
    """What hydo-build writes for this manifest, without building (no tools, no images)."""
    text = open(path).read()
    m = json.loads(text) if path.endswith(".json") else hb.parse_yaml(text, os.path.basename(path))
    if not isinstance(m, dict) or "services" not in m and "test" not in m and "workload" not in m:
        return None
    hb.apply_params(m, [])
    comp = hb._compose_expand(m, path)
    hb.validate(m)
    if comp:
        m["_compose"] = comp
    for s in m["services"]:          # an image service's cmd comes from its image (hydo_oci.prepare)
        s.setdefault("cmd", "/etc/hydo/oci/%s/run" % s["name"])
    fc = ("/bin/hydo_fc", "IPT/bin/iptables-legacy") if hb.has_freeze(m) else None
    faults = set(m.get("faults") or hb.DEFAULT_FAULT_KINDS)
    want_tc = bool(faults & {"network", "oneway"})
    if new:
        assert not hb.has_netns(m), path
        ms, pre = hb.compile_shell(m, "TC/bin/tc" if want_tc else "", fc, None)
        hosts = hb.default_hosts(m)
        staged = (want_tc or hb.has_netns(m), "freeze" in faults,
                  (hb.NETMODS if want_tc else []) + (hb.FCMODS if fc else []) + (hb.NSMODS if hb.has_netns(m) else []))
    else:
        ms, pre = hb.compile_shell(m, "TC/bin/tc" if want_tc else "", fc)
        hosts = "127.0.0.1 localhost\n"
        staged = (want_tc, "freeze" in faults, (hb.NETMODS if want_tc else []) + (hb.FCMODS if fc else []))
    custom = any(v.get("type") == "eventually" for v in m["invariants"]) or hb.has_links(m) or hb.has_freeze(m) or \
        "clock_after_cut" in (m.get("guest") or {}) or hb.has_buggify(m) or m.get("checks") or hb.has_world(m)
    init = hb.init_text(m) if custom else open(os.path.join(os.path.dirname(hb.__file__), "hydo-init.sh")).read()
    world = hb.world_config(m) if hb.has_world(m) else None
    return ms, pre, init, world, hosts, staged


def describe_and_draws(path):
    out = []
    for M, MS in ((EX_A, MS_A), (EX_B, MS_B)):
        try:
            sp = MS.load(manifest=path)
        except SystemExit as e:
            out.append(("exit", str(e)))
            continue
        out.append(("ok", json.dumps(sp.describe(), sort_keys=True), sp, M))
    return out


SAVED = {}
GLOBALS = ("SVCS", "NET_SVCS", "PAUSE_SVCS", "NORDERS", "KNOBS", "KNOB_TOKENS", "SPACE", "FC_PAIRS", "FC_MATCH", "PF_TARGETS")
for M in (EX_A, EX_B):
    SAVED[M] = {k: getattr(M, k) for k in GLOBALS if hasattr(M, k)}


def compose(M, strat, sd, dur):
    """explore.py main's composition of one schedule (main: items = ...)."""
    s = M.GENERATORS[strat](sd)
    if dur:
        s = M.durable_only(s)
    if strat not in ("swarm-pos", "process"):
        s = M.add_new_faults(s, sd, strat == "swarm")
    s = M.add_grpc_faults(M.add_protocol_faults(M.add_freeze_call(M.add_more_faults(s, sd, strat), sd), sd), sd)
    return M.render(M.add_compose(M.add_turns(M.add_buggify(M.add_preempt(M.add_concurrency(M.add_timing(s, sd), sd), sd), sd), sd), sd))


FLAGS = ("NEW_FAULTS", "TIMING", "CONCURRENCY", "PREEMPT", "BUGGIFY", "MORE_FAULTS", "FREEZE_CALL", "PROTOCOL_FAULTS")
COMBOS = [c for i, c in enumerate(itertools.product((False, True), repeat=len(FLAGS) + 1)) if i % 17 == 0] + \
         [(False,) * (len(FLAGS) + 1), (True,) * (len(FLAGS) + 1)]


def draws(spa, spb, seeds=range(1, 101)):
    n = 0
    for M, sp in ((EX_A, spa), (EX_B, spb)):
        for k, v in SAVED[M].items():
            setattr(M, k, v)
        sp.apply(vars(M))
        M.CF_RANGE = (1, 300)
    strats = [g for g in sorted(EX_A.GENERATORS) if not (g == "swarm-pos" and spa.steps < 15)]
    assert strats == [g for g in sorted(EX_B.GENERATORS) if not (g == "swarm-pos" and spb.steps < 15)]
    for strat in strats:
        for c in COMBOS:
            for M in (EX_A, EX_B):
                for f, v in zip(FLAGS, c):
                    setattr(M, f, v)
                if hasattr(M, "GRPC_FAULTS"):
                    M.GRPC_FAULTS = c[-2]
            a = [compose(EX_A, strat, sd, c[-1]) for sd in seeds]
            b = [compose(EX_B, strat, sd, c[-1]) for sd in seeds]
            assert a == b, (strat, c)
            assert [EX_A.finding_class(EX_A.parse(x)) for x in a] == [EX_B.finding_class(EX_B.parse(x)) for x in b]
            assert [[EX_A.render(y) for y in EX_A.candidates(EX_A.parse(x))] for x in a[:20]] == \
                   [[EX_B.render(y) for y in EX_B.candidates(EX_B.parse(x))] for x in b[:20]], (strat, c)
            n += len(a)
    return n


def main():
    nb = nd = ns = 0
    skipped = []
    for path in manifests(REF):
        rel = os.path.relpath(path, REF)
        try:
            ca = compiled(HB_A, path, False)
        except Exception as e:   # not a buildable manifest by itself (a fragment, a composer file)
            ea = repr(e)
            try:
                compiled(HB_B, path, True)
                raise AssertionError("%s: the reference refuses it (%s), the new hydo-build accepts it" % (rel, ea))
            except AssertionError:
                raise
            except Exception as e2:
                assert type(e2).__name__ == type(e).__name__ and str(e2) == str(e), (rel, ea, repr(e2))
            skipped.append(rel)
            continue
        if ca is None:
            skipped.append(rel)
            continue
        cb = compiled(HB_B, path, True)
        for what, x, y in zip(("manifest.sh", "prelude.sh", "/sbin/init", "world.json", "/etc/hosts", "staged"), ca, cb):
            assert x == y, "%s: %s differs" % (rel, what)
        nb += 1
        da, db = describe_and_draws(path)
        assert da[:2] == db[:2], "%s: manifest_space differs" % rel
        if da[0] == "ok":
            nd += 1
            ns += draws(da[2], db[2])
    print("(1) hydo-build: %d manifests compile byte-identically (manifest.sh, prelude.sh, /sbin/init, world.json, "
          "/etc/hosts, staged tools and modules); hydo-oci unchanged; %d files are not stand-alone manifests "
          "(same refusal from both): %s" % (nb, len(skipped), ", ".join(skipped) or "none"))
    print("(2) manifest_space: describe() identical on %d manifests" % nd)
    print("(3) explore.py: %d schedule strings identical (all generators, %d combinations of the optional streams "
          "and --durable-only, seeds 1-100; classes and shrink candidates too)" % (ns, len(COMBOS)))
    bridge = [p for p in manifests(NEW) if "network: bridge" in open(p).read()]
    for p in bridge:
        sp = MS_B.load(manifest=p)
        deps = {s["name"]: set(s.get("depends_on") or []) for s in sp.m["services"]}
        for l in sp.links:
            a, b = l.split("-")
            assert b == "*" or b in deps[a] or a in deps[b] or not any(deps.values()), (p, l)
        print("(4) %s (network: bridge): links %s" % (os.path.relpath(p, NEW), " ".join(sp.links)))


if __name__ == "__main__":
    main()
