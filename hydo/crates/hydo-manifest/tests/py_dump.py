#!/usr/bin/env python3
"""Test-only: what hydo-build (hydo/onboard/hydo-build) makes of a manifest, as one JSON line, for
hydo-manifest's equivalence test (tests/equivalence.rs).

    py_dump.py <manifest> [--arch A] [--set K=V ...]
    py_dump.py --embedded <manifest text file> [<compose.json>]     manifest_space.Space's view
    py_dump.py --batch        one request per stdin line: {"path", "arch", "sets"} or
                              {"embedded": <text file>, "compose": <json file or null>}

Output: {"ok": true, "data": <the manifest after main()'s steps up to the build>, "warnings": [the
`hydo-build: warning:` texts], "check": [the --check lines], "oci": <image services read>} or
{"ok": false, "error": <message>, "crash": <true when hydo-build would stop with a traceback>}.
The steps are main()'s, in its order: read (JSON by name, else parse_yaml), mapping check,
apply_params, _compose_expand, validate, _compose, check_ram_arch, service_order, _oci_prepare.
"""
import contextlib, importlib.machinery, importlib.util, io, json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
TOP = os.path.normpath(os.path.join(HERE, "..", "..", "..", ".."))
ONB = os.path.join(TOP, "hydo", "onboard")
EXPLORE = os.path.join(TOP, "hydo", "explore")


def _load():
    loader = importlib.machinery.SourceFileLoader("hydo_build", os.path.join(ONB, "hydo-build"))
    spec = importlib.util.spec_from_loader("hydo_build", loader)
    m = importlib.util.module_from_spec(spec)
    loader.exec_module(m)
    return m


hb = _load()
sys.path.insert(0, ONB)
sys.path.insert(0, EXPLORE)


def load(path, arch="x86_64", sets=()):
    err = io.StringIO()
    try:
        with contextlib.redirect_stderr(err):
            text = open(path).read()
            m = json.loads(text) if path.endswith(".json") else hb.parse_yaml(text, os.path.basename(path))
            if not isinstance(m, dict):
                raise hb.ManifestError("the manifest must be a mapping")
            hb.apply_params(m, list(sets))
            comp = hb._compose_expand(m, path)
            hb.validate(m)
            if comp:
                m["_compose"] = comp
            hb.check_ram_arch(m, arch)
            hb.service_order(m["services"])
            oci = hb._oci_prepare(m, path, arch if arch in ("x86_64", "arm64") else "x86_64")
    except (hb.YamlError, hb.ManifestError) as e:
        return {"ok": False, "error": str(e), "crash": False}
    except Exception as e:      # hydo-build stops here with a traceback
        return {"ok": False, "error": "%s: %s" % (type(e).__name__, e), "crash": True}
    warnings = [l[len("hydo-build: warning: "):] for l in err.getvalue().splitlines()
                if l.startswith("hydo-build: warning: ")]
    check = ["hydo-build: %s OK (%d services, %d invariants, %d workload steps)" % (
        path, len(m["services"]), len(m["invariants"]), m["workload"]["steps"]) +
        (" + %d history checks" % len(m["checks"]) if m.get("checks") else "")]
    if m.get("_compose"):
        c = m["_compose"]
        check.append("hydo-build: test composer: %d commands (%s), %d checks, %d regressions" % (
            len(c["commands"]), ", ".join(x["id"] for x in c["commands"]), len(c["checks"]), len(c["regressions"])))
    return {"ok": True, "data": m, "warnings": warnings, "check": check, "oci": bool(oci)}


def embedded(text_path, compose_path=None):
    import manifest_space
    try:
        text = open(text_path).read()
        compose = json.load(open(compose_path)) if compose_path else None
        # Space.__init__ up to validate (the rest is the explorer's fault space)
        m = json.loads(text) if text.lstrip().startswith("{") else hb.parse_yaml(text, "hydo.yaml")
        meta = None
        if compose is not None or "test" in m:
            hc = manifest_space._composer()
            if compose is not None:
                m["test"] = hc.test_from_meta(compose)
            meta = hc.expand(m, hb.ManifestError, None)
        hb.validate(m)
    except (hb.YamlError, hb.ManifestError) as e:
        return {"ok": False, "error": str(e), "crash": False}
    except Exception as e:
        return {"ok": False, "error": "%s: %s" % (type(e).__name__, e), "crash": True}
    return {"ok": True, "data": m, "meta": meta}


def main():
    a = sys.argv[1:]
    if a and a[0] == "--batch":
        for line in sys.stdin:
            if not line.strip():
                continue
            r = json.loads(line)
            if "embedded" in r:
                out = embedded(r["embedded"], r.get("compose"))
            else:
                out = load(r["path"], r.get("arch", "x86_64"), [tuple(x) for x in r.get("sets", [])])
            print(json.dumps(out, default=str), flush=True)
        return 0
    if a and a[0] == "--embedded":
        print(json.dumps(embedded(a[1], a[2] if len(a) > 2 else None), default=str))
        return 0
    path, arch, sets, i = a[0], "x86_64", [], 1
    while i < len(a):
        if a[i] == "--arch":
            arch = a[i + 1]; i += 2
        elif a[i] == "--set":
            sets.append(tuple(a[i + 1].split("=", 1))); i += 2
        else:
            raise SystemExit("py_dump: unknown argument %s" % a[i])
    print(json.dumps(load(path, arch, sets), default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())
