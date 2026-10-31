//! RECOVERY-INPUTS: what a power cut hands to the recovery conversation.
//!
//! In a shell image the guest prints `hydo-explore-acked:` and `hydo-explore-failed:` lines, and
//! the engine copies them into the recovery boot's `oracle` command (3,584 bytes at most, M89).
//! The extensions carry more in that line as tokens among the ids: `clock=<epoch>`
//! (hydo-init-clock.sh) and the fake world's `w:` records (hydo-init-fakes.sh). Under host
//! control the run's controller keeps these values itself and hands them over as one
//! [`RecoveryInputs`]: [`super::ScheduleController::recovery_inputs`] after the run ended with
//! [`crate::Ending::PowerCut`], and [`super::ScheduleController::recovery_from`] for the recovery
//! conversation, which writes the ids into the recovery VM's files itself. The engine only has to
//! cut the power, keep the flushed data disk and resume S0 (step C1); nothing has to fit in one
//! console line.
//!
//! A carried value is a field here, never a token among the ids: `acked` and `failed` hold ids
//! only. The fake world's records (M125) are `world`, as in [`crate::Ending::PowerCut`].

/// The recovery conversation's inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryInputs {
    /// The acknowledged ids, in the order of the first boot's acked file.
    pub acked: Vec<String>,
    /// The ids whose last outcome was a definite failure, in `sort -n` order.
    pub failed: Vec<String>,
    /// `guest.clock_after_cut`: the first boot's realtime clock at the cut, seconds since 1970
    /// (None without the feature, or when the clock could not be read: the recovery boot then
    /// starts from guest.clock_start, as hydo-init-clock.sh does for a missing token).
    pub clock: Option<i64>,
    /// `fakes:`/`proxies:`: the fake world's records the first boot carried (`w:` lines, the
    /// `world` of [`crate::Ending::PowerCut`]; empty without a world).
    pub world: Vec<String>,
}

/// The values a recovery conversation is given besides the ids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Carried {
    pub clock: Option<i64>,
}
