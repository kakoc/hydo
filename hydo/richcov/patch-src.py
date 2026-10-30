#!/usr/bin/env python3
"""patch-src.py <front-door source dir> <dest dir> -- the richcov variant of a front door.

Copies the Go source tree and adds the two signals of hydo/richcov/DESIGN.md section 2 that
need the guest's help: (1) the oracle prints one HYDO-STATE line per distinct per-order state
tuple (state.go.in); (2) the assertion SDK re-emits an outcome when its bucketed details are
new (distinct.go.in). Nothing else changes; works on the pre-fix explorer copy
(hydo/explore/stack-src-sdk) and on the fixed front door (hydo/stack)."""
import os, shutil, sys
src, dst = sys.argv[1], sys.argv[2]
here = os.path.dirname(os.path.abspath(__file__))
shutil.rmtree(dst, ignore_errors=True)
shutil.copytree(src, dst)
m = os.path.join(dst, "main.go")
s = open(m).read()
a = "\tpStoresReadable.Check(true, nil)\n"
assert s.count(a) == 1, "oracle anchor"
s = s.replace(a, a + "\trichState(n, mysqlHas, mysqlPub, kafkaHas, kafkaCount, rc)\n")
open(m, "w").write(s)
shutil.copy(os.path.join(here, "state.go.in"), os.path.join(dst, "richstate.go"))
h = os.path.join(dst, "hydoassert", "hydoassert.go")
s = open(h).read()
b = "\tif a.seen[i] {\n\t\treturn\n\t}\n\ta.seen[i] = true\n"
assert s.count(b) == 1, "sdk anchor"
s = s.replace(b, "\tif !richNew(a, i, d) {\n\t\treturn\n\t}\n\ta.seen[i] = true\n")
open(h, "w").write(s)
shutil.copy(os.path.join(here, "distinct.go.in"), os.path.join(dst, "hydoassert", "richdistinct.go"))
print("patched", dst)
