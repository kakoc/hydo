"""network: bridge (hydo-init-netns.sh, MANIFEST.md "Service networks"): validation, compiled
shell, init splice, default /etc/hosts, hydo-oci, fakes on the bridge and the explorer's links.
Run: python3 -m unittest tests.test_netns (from hvx/onboard)."""
import copy, importlib.machinery, importlib.util, os, shutil, subprocess, sys, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ONB = os.path.dirname(HERE)


def _load(name, path):
    spec = importlib.util.spec_from_loader(name, importlib.machinery.SourceFileLoader(name, path))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


hb = _load("hydo_build_t", os.path.join(ONB, "hydo-build"))
sys.path.insert(0, ONB)
import hydo_oci  # noqa: E402
sys.path.insert(0, os.path.join(os.path.dirname(ONB), "explore"))
import manifest_space  # noqa: E402

BASE = {
    "name": "t", "network": "bridge",
    "services": [
        {"name": "db", "cmd": "db", "ready": "true", "port": 5432, "addr": "10.89.0.11"},
        {"name": "api", "cmd": "api", "ready": "true", "port": 80, "addr": "10.89.0.12", "depends_on": ["db"]},
        {"name": "web", "cmd": "web", "ready": "true", "port": 80, "addr": "10.89.0.13", "depends_on": ["api"]},
    ],
    "workload": {"steps": 10, "run": "true"},
    "invariants": [{"id": "x", "cmd": "true"}],
}


def M(**kw):
    m = copy.deepcopy(BASE)
    m.update(kw)
    return m


class Validate(unittest.TestCase):
    def ok(self, m):
        return hb.validate(m)

    def bad(self, m, text):
        with self.assertRaises(hb.ManifestError) as c:
            hb.validate(m)
        self.assertIn(text, str(c.exception))

    def test_ok_and_same_port_twice(self):
        self.ok(M())                                    # api and web both on port 80: own namespaces

    def test_modes(self):
        self.ok(M(network="loopback", services=[dict(s, addr="127.0.0.%d" % (11 + i)) for i, s in enumerate(BASE["services"])]))
        self.bad(M(network="host"), "network: loopback or bridge")

    def test_addr_required_and_in_subnet(self):
        m = M(); del m["services"][0]["addr"]
        self.bad(m, "addr: required")
        m = M(); m["services"][0]["addr"] = "127.0.0.11"
        self.bad(m, "with network: bridge, an address in 10.89.0.0/16")
        m = M(); m["services"][0]["addr"] = "10.89.0.1"
        self.bad(m, "10.89.0.1")
        m = M(); m["services"][1]["addr"] = "10.89.0.11"
        self.bad(m, "used by two services")

    def test_loopback_unchanged(self):
        m = M(); del m["network"]
        self.bad(m, "must be a loopback address 127.x.y.z other than 127.0.0.1")

    def test_world_on_the_bridge(self):
        self.ok(M(proxies=[{"name": "dbp", "port": 6000, "service": "db", "protocol": "tcp"}]))
        self.ok(M(fakes=[{"name": "pay", "port": 9000, "addr": "10.89.0.50", "routes": [{"name": "r", "path": "/"}]}]))
        self.bad(M(fakes=[{"name": "pay", "port": 9000, "addr": "127.0.0.1", "routes": [{"name": "r", "path": "/"}]}]),
                 "with network: bridge, an address in 10.89.0.0/16")
        self.bad(M(fakes=[{"name": "pay", "port": 9000, "addr": "10.89.0.11", "routes": [{"name": "r", "path": "/"}]}]),
                 "is a service's address")
        self.bad(M(proxies=[{"name": "dbp", "port": 6000, "to": "127.0.0.1:5432", "protocol": "tcp"}]),
                 "<bridge address 10.89.x.y>:<port>")


class Compile(unittest.TestCase):
    def test_shell_and_hosts(self):
        m = hb.validate(M(fakes=[{"name": "pay", "port": 9000, "addr": "10.89.0.50", "routes": [{"name": "r", "path": "/"}]}]))
        ms, _ = hb.compile_shell(m, "TC", None, "IP")
        for line in ("HYDO_NET=bridge", "HYDO_IP='IP'", "HYDO_NET_GW=10.89.0.1", "HYDO_NET_EXTRA='10.89.0.50'"):
            self.assertIn(line + "\n", ms)
        h = hb.default_hosts(m)
        self.assertIn("10.89.0.11 db\n", h)
        self.assertIn("10.89.0.50 pay\n", h)
        self.assertIn('"addr": "10.89.0.50"', hb.world_config(m))

    def test_no_network_key_compiles_as_before(self):
        m = M(); del m["network"]
        for s in m["services"]:
            del s["addr"]
        hb.validate(m)
        ms, _ = hb.compile_shell(m, "TC", None, "IP")
        self.assertNotIn("HYDO_NET", ms)
        self.assertEqual(hb.default_hosts(m), "127.0.0.1 localhost\n")

    def test_init_splice(self):
        m = hb.validate(M(faults=["kill", "network", "freeze"]))
        t = hb.init_text(m)
        self.assertEqual(t.count("_ns_orig_start_svc() {"), 1)
        self.assertEqual(t.count("_ns_orig_fc_arm() {"), 1)
        self.assertLess(t.index("hydo-init-netns.sh"), t.index("# ---------------------------------------------------------------- main\n"))
        self.assertGreater(t.index("ns_setup() {"), t.index("hydo-init-freeze.sh"))     # last: it overrides fc_rules
        sh = shutil.which("sh")
        if sh:
            self.assertEqual(subprocess.run([sh, "-n"], input=t.encode()).returncode, 0)
        t2 = hb.init_text(hb.validate(M()))
        self.assertNotIn("_ns_orig_fc_arm() {", t2)

    def test_hydo_oci(self):
        self.assertIn("exec /etc/hydo/oci/$s/exec", hydo_oci.HYDO_OCI)
        self.assertNotIn("nsenter", hydo_oci.HYDO_OCI)
        self.assertIn("nsenter -n/tmp/hydo/ns/$s /etc/hydo/oci/$s/exec", hydo_oci.HYDO_OCI_NETNS)


class Links(unittest.TestCase):
    def space(self, m):
        import json
        return manifest_space.Space(json.dumps(m), "hydo.json")

    def test_talking_links_only(self):
        self.assertEqual(self.space(M()).links, ["db-api", "api-web", "api-*"])

    def test_peers_without_depends_on(self):
        m = M()
        for s in m["services"]:
            s.pop("depends_on", None)
        self.assertEqual(self.space(m).links, ["db-api", "db-web", "api-web", "db-*", "api-*", "web-*"])

    def test_loopback_keeps_every_pair(self):
        m = M(); del m["network"]
        for i, s in enumerate(m["services"]):
            s["addr"] = "127.0.0.%d" % (11 + i)
        self.assertEqual(self.space(m).links, ["db-api", "db-web", "api-web", "db-*", "api-*", "web-*"])


if __name__ == "__main__":
    unittest.main()
