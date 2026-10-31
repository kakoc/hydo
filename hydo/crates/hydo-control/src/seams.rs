//! The features of today's guest init that the core controller does not drive yet: one seam per
//! former init extension or fault kind, for wave 3 (docs/design/guest-control.md, step B2).
//!
//! A manifest or a schedule that needs one is refused with [`Unsupported`] naming the seam, never
//! run with the feature silently missing. Each seam is also marked in `schedule.rs` with a
//! `SEAM(<name>)` comment at the place where its code goes, so a feature controller can be added
//! there without moving anything else. [`Seam::wave3`] says, per seam, what the shell does today
//! and what the controller has to do.

use std::fmt;

/// One feature for wave 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Seam {
    /// `eventually` invariants (hydo-liveness.sh, M59).
    Liveness,
    /// `T=` clock steps and `guest.clock_after_cut` (hydo-init.sh's T= branch, hydo-init-clock.sh).
    Clock,
    /// History recording and its `final:` read-backs (`checks:`, hydo-init-history.sh, M87).
    History,
    /// `F=` freezes at a packet (`faults: [freeze]`, hydo-init-freeze.sh, M70).
    Freeze,
    /// `B=` BUGGIFY with `buggify: true` (hydo-init-buggify.sh, M56/M80).
    Buggify,
    /// Fake outside services, proxies and `H=` (hydo-init-fakes.sh, M89/M90).
    Fakes,
    /// The error-log scan (`errors:`, hydo-init-errlog.sh, M94).
    ErrorLogs,
    /// Per-service network namespaces (`network: bridge`, hydo-init-netns.sh, M95).
    NetNamespaces,
    /// The test composer's rounds (`test:` / the `hydo/` folder, `W=`, hydo-composer.sh, M86).
    Composer,
    /// `N=` / `O=` netem faults on a port or a link, their heal and drain (hydo-init.sh's
    /// net_on/net_heal, hydo-init-links.sh, M71).
    NetFaults,
    /// What the recovery conversation is given beyond the acknowledged and failed ids: the
    /// carried fake world, the first boot's clock, and how the engine hands a power cut over.
    RecoveryInputs,
}

impl Seam {
    pub const ALL: [Seam; 11] = [
        Seam::Liveness,
        Seam::Clock,
        Seam::History,
        Seam::Freeze,
        Seam::Buggify,
        Seam::Fakes,
        Seam::ErrorLogs,
        Seam::NetNamespaces,
        Seam::Composer,
        Seam::NetFaults,
        Seam::RecoveryInputs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Seam::Liveness => "liveness",
            Seam::Clock => "clock",
            Seam::History => "history",
            Seam::Freeze => "freeze",
            Seam::Buggify => "buggify",
            Seam::Fakes => "fakes",
            Seam::ErrorLogs => "error-logs",
            Seam::NetNamespaces => "net-namespaces",
            Seam::Composer => "composer",
            Seam::NetFaults => "net-faults",
            Seam::RecoveryInputs => "recovery-inputs",
        }
    }

    /// What the shell does today and what the controller has to do (one paragraph per seam).
    pub fn wave3(self) -> &'static str {
        match self {
            Seam::Liveness => {
                "hydo-liveness.sh wraps step_range, start_periodic, start_all and finish: an \
                 `eventually` invariant is polled every `poll` s after its anchor (`start` or \
                 `heal`) until it holds or `within` s pass. Controller: anchor at the stop where \
                 the anchor's command was acknowledged; one `probe` with every_ms = poll and \
                 deadline_ms = within; the HYDO-ASSERT eventually records from the probe event."
            }
            Seam::Clock => {
                "hydo-init.sh's T= branch steps the realtime clock before a step (date -s) and \
                 checks it within 5 s; hydo-init-clock.sh appends `clock=<epoch>` to the acked \
                 line at a cut and starts the recovery boot at that epoch + clock_after_cut. \
                 Controller: `clock_set {step}` at the point (its reply gives the new time, so \
                 the 5 s check is host-side), the fault.clock_* records; at a cut `clock_get`, \
                 carried to the recovery conversation as a value, not as a token in the ids."
            }
            Seam::History => {
                "hydo-init-history.sh wraps start_periodic and finish: it prints the history \
                 records and runs each check's `final:` read-back. Controller: `tail` the \
                 history file (line events) or keep the console records; run the read-backs \
                 before final_verdict; the checks stay host-side (hydo-history)."
            }
            Seam::Freeze => {
                "hydo-init-freeze.sh parses F= (a knob), arms hydo_fc before the step (iptables \
                 rules + the helper), heals in restart_dead and kills the helper at a cut. \
                 Controller: `run` the pinned iptables, then the agent's `freeze` command, the \
                 `froze`/`resumed` events, `unfreeze` at the heal; the fault.freeze_call records."
            }
            Seam::Buggify => {
                "hydo-init-buggify.sh turns the knob B= into /tmp/hydo-buggify (mode 644). \
                 Controller: one `write` of the value before the steps, at the knob's place."
            }
            Seam::Fakes => {
                "hydo-init-fakes.sh starts hydo-world before the services, turns H= into its \
                 rules, prints its summary, carries the world across a power cut on the acked \
                 line. Controller: `spawn` hydo-world, its ctl through `run`, `read` the world \
                 file at a cut and `write` it into the recovery VM."
            }
            Seam::ErrorLogs => {
                "hydo-init-errlog.sh scans each service's log at the end of every boot and \
                 prints HYDO-ERRLOG records. Controller: `tail` the logs (or `read` them at the \
                 end) and classify host-side with hydo-errors."
            }
            Seam::NetNamespaces => {
                "hydo-init-netns.sh gives each service its own namespace on a bridge before \
                 start_svc. Controller: the pinned `ip` through `run` for the plan, then `spawn` \
                 with `netns`."
            }
            Seam::Composer => {
                "The composer's W= knob writes the composition for /etc/hydo/compose.sh, whose \
                 rounds start commands in the guest. Controller: the composition is computed \
                 host-side already (hydo-composer); each command `spawn`ed at its round with \
                 HYDO_SEED/HYDO_CMD_ID/HYDO_ROUND, `exit` events, the drain and the kills."
            }
            Seam::NetFaults => {
                "hydo-init.sh's net_on/net_heal (and hydo-init-links.sh on links) configure \
                 netem with tc for [from, to) and drain the queue at the heal (M71). Controller: \
                 the same tc lines through `run`, `qlen` polls for the drain, the heal points \
                 (nto, oto, and at the end before restart_dead), the fault.oneway_* records."
            }
            Seam::RecoveryInputs => {
                "The engine copies the acked/failed lines into the recovery boot's `oracle` \
                 command (3,584 bytes, M89). The core controller writes the ids itself in the \
                 recovery conversation (`write`, chunked); still to do: the engine side of the \
                 hand-over (C1), the first boot's clock (Clock) and the carried world (Fakes)."
            }
        }
    }
}

impl fmt::Display for Seam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A manifest or schedule needs a feature the core controller does not drive yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsupported {
    pub seam: Option<Seam>,
    pub what: String,
}

impl Unsupported {
    pub fn seam(seam: Seam, what: impl Into<String>) -> Unsupported {
        Unsupported {
            seam: Some(seam),
            what: what.into(),
        }
    }

    /// Not a wave-3 feature: the input cannot be driven at all (a manifest hydo-build refuses).
    pub fn other(what: impl Into<String>) -> Unsupported {
        Unsupported {
            seam: None,
            what: what.into(),
        }
    }
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.seam {
            Some(s) => write!(f, "{} (not driven by the core controller yet: seam {s})", self.what),
            None => f.write_str(&self.what),
        }
    }
}

impl std::error::Error for Unsupported {}
