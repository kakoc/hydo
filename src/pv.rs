//! The hydo paravirtual device: a small set of I/O ports through which the guest
//! draws deterministic randomness and time, records checkpoints, and requests
//! shutdown. This is the guest's sanctioned channel for everything that would
//! otherwise be nondeterministic.
//!
//! Port map (all accessed with the port in DX):
//!   0x520  IN  (32-bit)  next PRNG word; advances the PRNG
//!   0x524  IN  (32-bit)  low 32 bits of the virtual clock
//!   0x528  OUT (32-bit)  request shutdown with the written exit code
//!   0x52c  OUT (32-bit)  record a checkpoint value into the trace

use crate::det::{Clock, SplitMix64};

pub const PV_RANDOM: u16 = 0x520;
pub const PV_TIME: u16 = 0x524;
pub const PV_EXIT: u16 = 0x528;
pub const PV_CHECK: u16 = 0x52c;

pub const PV_RANGE: std::ops::Range<u16> = PV_RANDOM..PV_CHECK + 4;

pub struct Pv {
    pub prng: SplitMix64,
    pub clock: Clock,
    /// Values the guest explicitly recorded. This is the guest-visible trace the
    /// determinism harness compares across runs.
    pub checkpoints: Vec<u32>,
    /// Set when the guest writes the exit port; the run loop stops.
    pub exit: Option<u32>,
}

impl Pv {
    pub fn new(seed: u64) -> Self {
        Pv {
            prng: SplitMix64::new(seed),
            clock: Clock::new(),
            checkpoints: Vec::new(),
            exit: None,
        }
    }

    pub fn handles(port: u16) -> bool {
        PV_RANGE.contains(&port)
    }

    pub fn io_in(&mut self, port: u16, data: &mut [u8]) {
        let val: u32 = match port {
            PV_RANDOM => self.prng.next_u32(),
            PV_TIME => self.clock.now() as u32,
            _ => 0,
        };
        write_le(data, val);
    }

    pub fn io_out(&mut self, port: u16, data: &[u8]) {
        let val = read_le(data);
        match port {
            PV_CHECK => self.checkpoints.push(val),
            PV_EXIT => self.exit = Some(val),
            _ => {}
        }
    }
}

fn write_le(data: &mut [u8], val: u32) {
    let bytes = val.to_le_bytes();
    for (i, b) in data.iter_mut().enumerate() {
        *b = bytes.get(i).copied().unwrap_or(0);
    }
}

fn read_le(data: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    for (i, b) in data.iter().take(4).enumerate() {
        buf[i] = *b;
    }
    u32::from_le_bytes(buf)
}
