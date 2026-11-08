//! Work counters of a run (an experiment, docs/design/cost-tracking.md; `HYDO_COST`).
//!
//! A run is deterministic, so what the guest did can be counted exactly: disk requests, flushes
//! and sectors (`/proc/diskstats`), packets and bytes per interface (`/proc/net/dev`), CPU ticks,
//! interrupts, context switches and forks (`/proc/stat`), page faults and written-back pages
//! (`/proc/vmstat`), memory (`/proc/meminfo`) and, per process name, CPU ticks, page faults,
//! resident memory and, where the kernel keeps them, read and write system calls and bytes
//! (`/proc/<pid>/io`). [`probe`] reads them with one busybox script at a defined point of the
//! conversation and reports each line as `HYDO-COST <label> <line>`; the control transcript
//! records the report at its stop, whose `at` is the engine's clock there (exits on kvm and hvm,
//! the instruction clock on hvx). It runs only when the host asked for it
//! ([`crate::plan::Plan`]'s `cost`), so a run without it sends the same commands as before.
//!
//! The probe is guest work itself (a shell, a few awk processes), the same in every run of a
//! schedule; it moves the guest's clock and so the fingerprints of the runs it is on.

use super::{bb, log, run, sh, text::read_bytes, Ctx, F};
use crate::flow::Step;

/// The probe script: every counter as one line per item, into `out`.
fn script(b: &str, out: &str) -> String {
    format!(
        r#"B={b}
{{
read -r up _ < /proc/uptime; echo "uptime $up"
$B awk '$4+$8+$19>0{{print "disk",$3,"rd="$4,"rsec="$6,"wr="$8,"wsec="$10,"fl="$19}}' /proc/diskstats
$B awk 'NR>2{{sub(":"," ");if($3+$11>0)print "net",$1,"rxp="$3,"rxb="$2,"txp="$11,"txb="$10}}' /proc/net/dev
$B awk '/^cpu /{{print "cpu","user="$2,"nice="$3,"sys="$4,"idle="$5,"iow="$6,"irq="$7,"sirq="$8}}/^ctxt /{{print "ctxt",$2}}/^processes /{{print "forks",$2}}/^intr /{{print "intr",$2}}' /proc/stat
$B awk '$1~/^(pgfault|pgmajfault|nr_dirtied|nr_written|pgpgin|pgpgout)$/{{print "vm",$1"="$2}}' /proc/vmstat
$B awk '$1~/^(MemTotal|MemFree|Cached|Dirty|AnonPages|Shmem|Slab):$/{{sub(":","",$1);print "mem",$1"="$2}}' /proc/meminfo
for d in /proc/[0-9]*; do
  read -r s < $d/stat || continue
  io=; if [ -r $d/io ]; then while read -r k v; do io="$io $v"; done < $d/io; fi
  echo "$s @$io"
done 2>/dev/null | $B awk '{{c=$2;gsub(/[()]/,"",c);if($23==0){{sub(/\/.*/,"",c);c="k:"c}}
 for(i=1;i<=NF;i++)if($i=="@")a=i
 n[c]++;t[c]+=$14+$15;mf[c]+=$10;jf[c]+=$12;rss[c]+=$24*4
 rc[c]+=$(a+1);wc[c]+=$(a+2);sr[c]+=$(a+3);sw[c]+=$(a+4);wb[c]+=$(a+6)}}
 END{{for(c in n)print "proc",c,"n="n[c],"cpu="t[c],"minflt="mf[c],"majflt="jf[c],"rss_kb="rss[c],"rchar="rc[c],"wchar="wc[c],"syscr="sr[c],"syscw="sw[c],"wbytes="wb[c]}}' | $B sort
}} > {out} 2>&1"#
    )
}

/// Read the counters and report them under `label`, when the host asked for it: with
/// `HYDO_COST=1` only the probe after the verdict (`full_only` false), with `HYDO_COST=full` also
/// the ones before it (the workload's start and end, a recovery boot before its checks), which
/// are guest work inside the run and so move its timing: they can change a timing-sensitive
/// verdict (docs/design/cost-tracking.md).
pub(super) fn probe(fx: &mut F<'_>, ctx: &Ctx, label: &str, full_only: bool) -> Step<()> {
    if ctx.plan.cost == 0 || (full_only && ctx.plan.cost < 2) {
        return Ok(());
    }
    let out = format!("{}/cost.{label}", ctx.plan.layout.dir);
    let ran = run(fx, sh(ctx, script(&bb(ctx), &out)), Vec::new(), None)?;
    if ran.status != 0 {
        log(fx, format!("cost probe {label} rc={}", ran.status));
    }
    let text = read_bytes(fx, &out)?;
    for l in String::from_utf8_lossy(&text).lines() {
        fx.report(format!("HYDO-COST {label} {l}"));
    }
    Ok(())
}
