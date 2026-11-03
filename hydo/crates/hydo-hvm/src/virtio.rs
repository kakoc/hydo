//! The virtio-mmio (version 2) transport and its two devices, ported from hv/hvm/virtio.c,
//! virtio_blk.c and virtio_net.c: a block device with hvx's seeded disk faults and the M26
//! volatile write cache, and a minimal network device with scripted, paced receive and captured
//! transmit. Everything is driven by guest MMIO exits and the virtual clock, and all state that is
//! not a buffer ([`VState`]) is hashed with its exact C layout. The C files were deleted in
//! Milestone 147 (`git show 145c456:hv/hvm/virtio_blk.c` and so on); their notes follow.
//!
//! A kick services the queue at once, and its completion interrupt is raised when the virtual
//! clock reaches the kick time plus the latency (the M16 discipline). All device state, the disk
//! image and the disk's volatile write cache are in the snapshot and the fingerprint.
//! `HVM_DISK_OUT` gets the durable image at the end: flushed bytes only, the volatile write cache
//! is lost, as at a power cut.
//!
//! The disk's faults give the same decisions as hvx's for the same seed: a request's 64-bit roll
//! is `fault_roll(seed, request index)` over every request (reads, writes, flushes), a fault
//! applies when `(roll % 100) < prob`, and the class is chosen among the enabled, applicable
//! classes in the fixed order DELAY, ERROR, DROP, TORN, DROP_UNFLUSHED by `((roll >> 8) % n)`.
//! REORDER permutes a kick's used-ring entries with the per-kick roll
//! `fault_mix((seed ^ 0xD15EA5E5C0FFEE11) + (kick+1)*GOLDEN)`. DELAY latencies are in exits of
//! virtual time (hvx: instructions); a completion is at kick time + (16 + summed delays) exits.
//! Two differences from hvx, both making the model more faithful: the volatile cache has no size
//! cap (hvx holds at most 256 writes of up to 8 KiB and persists anything larger), and request
//! types other than IN/OUT/FLUSH answer UNSUPP instead of being treated as writes.

use crate::fnv::{fnv, fnv_u64};
use crate::machine::{mix, GOLDEN};
use hydo_hvx::cstr;

/// device i at MMIO_BASE + i * MMIO_SIZE
pub const MMIO_BASE: u64 = 0x0a00_0000;
pub const MMIO_SIZE: u64 = 0x200;
/// device i uses SPI 16 + i (INTID 48 + i)
pub const SPI_BASE: u32 = 16;
const VQ_MAX: usize = 2;
const VQ_SIZE_MAX: u32 = 256;
const VDEV_NET: u32 = 1;
const VDEV_BLK: u32 = 2;
const NET_RX_MAX: usize = 64;
const NET_FRAME_MAX: usize = 1536;
const NET_TX_CAP: usize = 1 << 20;
/// hvx's HVX_VBLK_LATENCY / HVX_VNET_LATENCY, in exits of virtual time
const LATENCY_EXITS: u64 = 16;

pub const F_DELAY: u32 = 1 << 0;
pub const F_ERROR: u32 = 1 << 1;
pub const F_DROP: u32 = 1 << 2;
pub const F_REORDER: u32 = 1 << 3;
pub const F_TORN: u32 = 1 << 4;
pub const F_UNFLUSHED: u32 = 1 << 5;
const NF_DROP: u32 = 1 << 0;
const NF_CORRUPT: u32 = 1 << 2;
const NF_DUP: u32 = 1 << 3;

const VQ_F_NEXT: u16 = 1;
const VQ_F_WRITE: u16 = 2;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Vq {
    pub num: u32,
    pub ready: u32,
    pub desc: u64,
    pub avail: u64,
    pub used: u64,
    pub last_avail: u32,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct VDev {
    pub ty: u32,
    pub status: u32,
    pub dfsel: u32,
    pub drfsel: u32,
    pub qsel: u32,
    pub drv_features: u64,
    pub q: [Vq; VQ_MAX],
    /// interrupt cause latched at a kick, raised at `due`
    pub isr: u32,
    /// InterruptStatus as the guest sees it; the line is (isr_shown != 0)
    pub isr_shown: u32,
    pub armed: u32,
    pub due: u64,
    pub config_gen: u32,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct VBlk {
    pub seed: u64,
    pub classes: u32,
    pub prob: u32,
    pub max_delay: u32,
    pub pad: u32,
    pub req_seq: u64,
    pub kick_seq: u64,
    pub fault_count: u64,
    pub disk_size: u64,
    /// entries in the volatile write cache
    pub n_unsync: u64,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct VNet {
    pub mac: [u8; 6],
    pub pad: [u8; 2],
    pub rx_n: u32,
    pub rx_head: u32,
    pub rx_next_at: u64,
    pub pace: u64,
    pub seed: u64,
    pub classes: u32,
    pub prob: u32,
    pub rx_seq: u64,
    pub tx_seq: u64,
    pub fault_count: u64,
    pub tx_len: u64,
}

/// `struct vstate`: everything deterministic that is not a buffer.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct VState {
    pub ndev: u32,
    pub pad: u32,
    pub step: u64,
    pub dev: [VDev; 2],
    pub blk: VBlk,
    pub net: VNet,
}

/// sizeof(struct vstate) on arm64.
pub const VSTATE_SIZE: usize = 448;

struct B(Vec<u8>);
impl B {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn align8(&mut self) {
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
    }
}

impl VState {
    /// The C layout (padding zero, as the C state is memset and copied whole).
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = B(Vec::with_capacity(VSTATE_SIZE));
        b.u32(self.ndev);
        b.u32(self.pad);
        b.u64(self.step);
        for d in &self.dev {
            for v in [d.ty, d.status, d.dfsel, d.drfsel, d.qsel] {
                b.u32(v);
            }
            b.u64(d.drv_features);
            for q in &d.q {
                b.u32(q.num);
                b.u32(q.ready);
                b.u64(q.desc);
                b.u64(q.avail);
                b.u64(q.used);
                b.u32(q.last_avail);
                b.align8();
            }
            b.u32(d.isr);
            b.u32(d.isr_shown);
            b.u32(d.armed);
            b.u64(d.due);
            b.u32(d.config_gen);
            b.align8();
        }
        let k = &self.blk;
        b.u64(k.seed);
        for v in [k.classes, k.prob, k.max_delay, k.pad] {
            b.u32(v);
        }
        for v in [
            k.req_seq,
            k.kick_seq,
            k.fault_count,
            k.disk_size,
            k.n_unsync,
        ] {
            b.u64(v);
        }
        let n = &self.net;
        b.0.extend_from_slice(&n.mac);
        b.0.extend_from_slice(&n.pad);
        b.u32(n.rx_n);
        b.u32(n.rx_head);
        for v in [n.rx_next_at, n.pace, n.seed] {
            b.u64(v);
        }
        b.u32(n.classes);
        b.u32(n.prob);
        for v in [n.rx_seq, n.tx_seq, n.fault_count, n.tx_len] {
            b.u64(v);
        }
        b.align8();
        debug_assert_eq!(b.0.len(), VSTATE_SIZE);
        b.0
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct VStats {
    pub kicks: u64,
    pub reqs: u64,
    pub reads: u64,
    pub writes: u64,
    pub flushes: u64,
    pub faults: u64,
    pub held: u64,
    pub committed: u64,
    pub rx: u64,
    pub tx: u64,
}

/// One write held in the volatile cache.
#[derive(Clone, Debug)]
pub struct Unsync {
    pub doff: u64,
    pub len: u32,
    pub valid: u32,
    pub data: Vec<u8>,
}

/// How the devices are configured (virtio.h's environment variables, read once).
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub disk: Option<String>,
    pub disk_out: Option<String>,
    pub fault: Option<String>,
    pub fault_classes: Option<String>,
    pub fault_prob: Option<String>,
    pub fault_maxdelay: Option<String>,
    pub net: bool,
    pub net_rx: Option<String>,
    pub net_rx_pace: Option<String>,
    pub net_tx_out: Option<String>,
    pub net_fault: Option<String>,
    pub net_fault_classes: Option<String>,
    pub net_fault_prob: Option<String>,
}

/// A snapshot of the devices (`virtio_save`).
#[derive(Clone, Debug)]
pub struct Save {
    s: VState,
    disk: Option<Vec<u8>>,
    tx: Option<Vec<u8>>,
    cache: Vec<Unsync>,
}

pub struct Virtio {
    pub v: VState,
    pub vs: VStats,
    /// the stable-media image (flushed bytes)
    pub disk: Option<Vec<u8>>,
    /// the volatile write cache, insertion order (`v.blk.n_unsync` entries)
    pub cache: Vec<Unsync>,
    net_rx: Vec<Vec<u8>>,
    net_tx: Option<Vec<u8>>,
    pub blk_idx: i32,
    pub net_idx: i32,
}

fn parse_classes(s: Option<&str>, net: bool) -> u32 {
    let Some(s) = s else {
        return if net { NF_DROP } else { F_ERROR };
    };
    if s.as_bytes().first().is_some_and(|c| c.is_ascii_digit()) {
        return cstr::strtoull0(s.as_bytes()) as u32;
    }
    let mut c = 0;
    if net {
        if s.contains("drop") {
            c |= NF_DROP;
        }
        if s.contains("corrupt") {
            c |= NF_CORRUPT;
        }
        if s.contains("dup") {
            c |= NF_DUP;
        }
        return c;
    }
    if s.contains("delay") {
        c |= F_DELAY;
    }
    if s.contains("error") {
        c |= F_ERROR;
    }
    if s.contains("unflushed") {
        c |= F_UNFLUSHED;
    }
    if s.contains("drop") {
        c |= F_DROP;
    }
    if s.contains("reorder") {
        c |= F_REORDER;
    }
    if s.contains("torn") {
        c |= F_TORN;
    }
    c
}

/// The hvx fault PRNG (`fault_roll`).
fn fault_roll(seed: u64, idx: u64) -> u64 {
    mix(seed.wrapping_add((idx.wrapping_add(1)).wrapping_mul(GOLDEN)))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bf {
    None,
    Delay,
    Error,
    Drop,
    Torn,
    Unflush,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Nf {
    None,
    Drop,
    Corrupt,
    Dup,
}

const SECTOR: u64 = 512;
const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;
const S_OK: u8 = 0;
const S_IOERR: u8 = 1;
const S_UNSUPP: u8 = 2;
const REORDER_MAX: usize = 16;
const HDR_LEN: u32 = 12;
const RX_SALT: u64 = 0x0052_0000_0000_0000;
const TX_SALT: u64 = 0x0054_0000_0000_0000;

/// Guest memory as the devices see it: `grw` with its bounds rule.
pub struct Ram<'a> {
    pub b: &'a mut [u8],
    pub base: u64,
}

impl Ram<'_> {
    fn ok(&self, gpa: u64, len: u64) -> Option<usize> {
        let size = self.b.len() as u64;
        if gpa < self.base || gpa - self.base > size || len > size - (gpa - self.base) {
            return None;
        }
        Some((gpa - self.base) as usize)
    }
    /// guest -> buf
    fn read(&self, gpa: u64, buf: &mut [u8]) -> bool {
        match self.ok(gpa, buf.len() as u64) {
            Some(o) => {
                buf.copy_from_slice(&self.b[o..o + buf.len()]);
                true
            }
            None => false,
        }
    }
    /// buf -> guest
    fn write(&mut self, gpa: u64, buf: &[u8]) -> bool {
        match self.ok(gpa, buf.len() as u64) {
            Some(o) => {
                self.b[o..o + buf.len()].copy_from_slice(buf);
                true
            }
            None => false,
        }
    }
    fn r16(&self, gpa: u64) -> Option<u16> {
        let mut b = [0u8; 2];
        self.read(gpa, &mut b).then(|| u16::from_le_bytes(b))
    }
    fn w16(&mut self, gpa: u64, v: u16) -> bool {
        self.write(gpa, &v.to_le_bytes())
    }
    fn desc(&self, gpa: u64) -> Option<(u64, u32, u16, u16)> {
        let mut b = [0u8; 16];
        self.read(gpa, &mut b).then(|| {
            (
                u64::from_le_bytes(b[0..8].try_into().unwrap()),
                u32::from_le_bytes(b[8..12].try_into().unwrap()),
                u16::from_le_bytes(b[12..14].try_into().unwrap()),
                u16::from_le_bytes(b[14..16].try_into().unwrap()),
            )
        })
    }
    fn used(&mut self, q: &Vq, ui: u16, id: u32, len: u32) {
        let mut e = [0u8; 8];
        e[..4].copy_from_slice(&id.to_le_bytes());
        e[4..].copy_from_slice(&len.to_le_bytes());
        // C: `q->used + 4 + 8 * (ui % q->num)`, the product in 32 bits
        self.write(q.used + 4 + 8u32.wrapping_mul(ui as u32 % q.num) as u64, &e);
    }
}

impl Virtio {
    /// `virtio_setup`: the devices from the configuration (errors as the C engine's, status 1).
    pub fn setup(c: &Config) -> Result<Virtio, String> {
        let mut x = Virtio {
            v: VState::default(),
            vs: VStats::default(),
            disk: None,
            cache: Vec::new(),
            net_rx: Vec::new(),
            net_tx: None,
            blk_idx: -1,
            net_idx: -1,
        };
        if let Some(dp) = &c.disk {
            let d = std::fs::read(dp).map_err(|e| format!("{dp}: {e}"))?;
            if d.len() % 512 != 0 || d.is_empty() {
                return Err(format!("hvm: {dp}: size must be a nonzero multiple of 512"));
            }
            x.blk_idx = x.v.ndev as i32;
            x.v.ndev += 1;
            x.v.dev[x.blk_idx as usize].ty = VDEV_BLK;
            x.v.blk.disk_size = d.len() as u64;
            x.disk = Some(d);
            if let Some(f) = &c.fault {
                x.v.blk.seed = cstr::strtoull0(f.as_bytes());
                x.v.blk.classes = parse_classes(c.fault_classes.as_deref(), false);
                x.v.blk.prob = c
                    .fault_prob
                    .as_deref()
                    .map(|p| cstr::atoi(p.as_bytes()) as u32)
                    .unwrap_or(100);
                x.v.blk.max_delay = c
                    .fault_maxdelay
                    .as_deref()
                    .map(|p| cstr::atoi(p.as_bytes()) as u32)
                    .unwrap_or(8);
            }
        }
        if c.net {
            x.net_idx = x.v.ndev as i32;
            x.v.ndev += 1;
            x.v.dev[x.net_idx as usize].ty = VDEV_NET;
            x.v.net.mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
            x.v.net.pace = c
                .net_rx_pace
                .as_deref()
                .map(|p| cstr::strtoull0(p.as_bytes()))
                .unwrap_or(4_800_000);
            x.v.net.rx_next_at = x.v.net.pace;
            x.net_tx = Some(vec![0u8; NET_TX_CAP]);
            if let Some(rp) = &c.net_rx {
                let f = std::fs::read(rp).map_err(|e| format!("{rp}: {e}"))?;
                let mut p = 0;
                while x.net_rx.len() < NET_RX_MAX && p + 2 <= f.len() {
                    let n = ((f[p] as usize) << 8) | f[p + 1] as usize;
                    p += 2;
                    if n > NET_FRAME_MAX || p + n > f.len() {
                        return Err(format!("hvm: bad frame in {rp}"));
                    }
                    x.net_rx.push(f[p..p + n].to_vec());
                    p += n;
                }
                x.v.net.rx_n = x.net_rx.len() as u32;
            }
            if let Some(nf) = &c.net_fault {
                x.v.net.seed = cstr::strtoull0(nf.as_bytes());
                x.v.net.classes = parse_classes(c.net_fault_classes.as_deref(), true);
                x.v.net.prob = c
                    .net_fault_prob
                    .as_deref()
                    .map(|p| cstr::atoi(p.as_bytes()) as u32)
                    .unwrap_or(100);
            }
        }
        Ok(x)
    }

    /// `virtio_bind`'s part that is state: the clock step.
    pub fn bind(&mut self, step: u64) {
        self.v.step = step;
    }
    pub fn ndev(&self) -> usize {
        self.v.ndev as usize
    }
    pub fn attached(&self) -> bool {
        self.v.ndev > 0
    }
    /// (base, SPI) of device i.
    pub fn dev_info(i: usize) -> (u64, u32) {
        (MMIO_BASE + i as u64 * MMIO_SIZE, SPI_BASE + i as u32)
    }
    pub fn level(&self, i: usize) -> bool {
        self.v.dev[i].isr_shown != 0
    }

    fn complete(&mut self, i: usize, now: u64, extra_exits: u64) {
        let step = self.v.step;
        let d = &mut self.v.dev[i];
        d.isr |= 1;
        d.armed = 1;
        d.due = now.wrapping_add((LATENCY_EXITS + extra_exits).wrapping_mul(step));
    }

    fn vdev_reset(&mut self, i: usize) {
        let ty = self.v.dev[i].ty;
        self.v.dev[i] = VDev {
            ty,
            ..VDev::default()
        };
        if i as i32 == self.blk_idx {
            self.cache_clear();
        }
    }

    fn dev_features(&self, i: usize) -> u64 {
        (1u64 << 32)
            | if i as i32 == self.blk_idx {
                self.blk_features()
            } else {
                1 << 5
            }
    }

    /// `virtio_mmio`: true if the address was a device's.
    pub fn mmio(
        &mut self,
        ram: &mut Ram,
        ipa: u64,
        write: bool,
        size: u32,
        val: &mut u64,
        now: u64,
    ) -> bool {
        if ipa < MMIO_BASE || ipa >= MMIO_BASE + self.v.ndev as u64 * MMIO_SIZE {
            return false;
        }
        let i = ((ipa - MMIO_BASE) / MMIO_SIZE) as usize;
        let off = (ipa - MMIO_BASE) % MMIO_SIZE;
        let nq: u32 = if self.v.dev[i].ty == VDEV_NET { 2 } else { 1 };
        let qi = if (self.v.dev[i].qsel as usize) < VQ_MAX {
            self.v.dev[i].qsel as usize
        } else {
            0
        };
        if off >= 0x100 {
            if !write {
                *val = if i as i32 == self.blk_idx {
                    self.blk_config(off - 0x100, size)
                } else {
                    self.net_config(off - 0x100, size)
                };
            }
            return true;
        }
        if !write {
            let d = &self.v.dev[i];
            let q = &d.q[qi];
            let v: u32 = match off {
                0x000 => 0x7472_6976,
                0x004 => 2,
                0x008 => d.ty,
                0x00c => 0x6f64_7968,
                0x010 => match d.dfsel {
                    0 => self.dev_features(i) as u32,
                    1 => (self.dev_features(i) >> 32) as u32,
                    _ => 0,
                },
                0x034 => {
                    if d.qsel < nq {
                        VQ_SIZE_MAX
                    } else {
                        0
                    }
                }
                0x044 => q.ready,
                0x060 => d.isr_shown,
                0x070 => d.status,
                0x0fc => d.config_gen,
                _ => 0,
            };
            *val = v as u64;
            return true;
        }
        let v = *val as u32;
        match off {
            0x014 => self.v.dev[i].dfsel = v,
            0x020 => {
                let d = &mut self.v.dev[i];
                if d.drfsel < 2 {
                    let sh = 32 * d.drfsel;
                    d.drv_features =
                        (d.drv_features & !(0xffff_ffffu64 << sh)) | ((v as u64) << sh);
                }
            }
            0x024 => self.v.dev[i].drfsel = v,
            0x030 => self.v.dev[i].qsel = v,
            0x038 => {
                if self.v.dev[i].qsel < nq {
                    self.v.dev[i].q[qi].num = v;
                }
            }
            0x044 => {
                if self.v.dev[i].qsel < nq {
                    self.v.dev[i].q[qi].ready = v & 1;
                }
            }
            0x050 => {
                self.vs.kicks += 1;
                if i as i32 == self.blk_idx {
                    self.blk_notify(ram, i, now);
                } else {
                    self.net_notify(ram, i, v as i32, now);
                }
            }
            0x064 => self.v.dev[i].isr_shown &= !v,
            0x070 => {
                if v == 0 {
                    self.vdev_reset(i);
                } else {
                    self.v.dev[i].status = v;
                }
            }
            0x080 => {
                let q = &mut self.v.dev[i].q[qi];
                q.desc = (q.desc & !0xffff_ffffu64) | v as u64;
            }
            0x084 => {
                let q = &mut self.v.dev[i].q[qi];
                q.desc = (q.desc & 0xffff_ffff) | ((v as u64) << 32);
            }
            0x090 => {
                let q = &mut self.v.dev[i].q[qi];
                q.avail = (q.avail & !0xffff_ffffu64) | v as u64;
            }
            0x094 => {
                let q = &mut self.v.dev[i].q[qi];
                q.avail = (q.avail & 0xffff_ffff) | ((v as u64) << 32);
            }
            0x0a0 => {
                let q = &mut self.v.dev[i].q[qi];
                q.used = (q.used & !0xffff_ffffu64) | v as u64;
            }
            0x0a4 => {
                let q = &mut self.v.dev[i].q[qi];
                q.used = (q.used & 0xffff_ffff) | ((v as u64) << 32);
            }
            _ => {}
        }
        true
    }

    /// `virtio_poll`: completions that are due become visible.
    pub fn poll(&mut self, ram: &mut Ram, now: u64) {
        if self.net_idx >= 0 {
            self.net_poll(ram, now);
        }
        for i in 0..self.v.ndev as usize {
            let d = &mut self.v.dev[i];
            if d.armed != 0 && now >= d.due {
                d.isr_shown |= d.isr;
                d.isr = 0;
                d.armed = 0;
            }
        }
    }

    /// `virtio_next_due`: the earliest future event, 0 = none.
    pub fn next_due(&self, ram: &Ram, now: u64) -> u64 {
        let mut best = 0;
        for i in 0..self.v.ndev as usize {
            let d = &self.v.dev[i];
            if d.armed != 0 && (best == 0 || d.due < best) {
                best = d.due;
            }
        }
        if self.net_idx >= 0 {
            let n = self.net_next_due(ram, now);
            if n != 0 && (best == 0 || n < best) {
                best = n;
            }
        }
        best
    }

    /// `virtio_hash`: device state, disk image, volatile cache, captured TX.
    pub fn hash(&self, mut h: u64) -> u64 {
        h = fnv(&self.v.bytes(), h);
        if let Some(d) = &self.disk {
            h = fnv(&d[..self.v.blk.disk_size as usize], h);
        }
        for c in self.cache.iter().take(self.v.blk.n_unsync as usize) {
            h = fnv_u64(c.doff, h);
            h = fnv(&c.len.to_le_bytes(), h);
            h = fnv(&c.valid.to_le_bytes(), h);
            h = fnv(&c.data[..c.len as usize], h);
        }
        if let Some(t) = &self.net_tx {
            h = fnv(&t[..self.v.net.tx_len as usize], h);
        }
        h
    }

    pub fn save(&self) -> Save {
        Save {
            s: self.v,
            disk: self.disk.clone(),
            tx: self.net_tx.clone(),
            cache: self.cache[..self.v.blk.n_unsync as usize].to_vec(),
        }
    }

    pub fn restore(&mut self, s: &Save) {
        self.v = s.s;
        if let (Some(d), Some(sd)) = (&mut self.disk, &s.disk) {
            d.copy_from_slice(sd);
        }
        if let (Some(t), Some(st)) = (&mut self.net_tx, &s.tx) {
            t.copy_from_slice(st);
        }
        self.cache = s.cache.clone();
    }

    /// `virtio_finish`: HVM_DISK_OUT (stable media only) and HVM_NET_TX_OUT.
    pub fn finish(&self, c: &Config) {
        if let (Some(o), Some(d)) = (&c.disk_out, &self.disk) {
            if let Err(e) = std::fs::write(o, &d[..self.v.blk.disk_size as usize]) {
                eprintln!("{o}: {e}");
            }
        }
        if let (Some(o), Some(t)) = (&c.net_tx_out, &self.net_tx) {
            let _ = std::fs::write(o, &t[..self.v.net.tx_len as usize]);
        }
    }

    pub fn stats(&self, tag: &str) -> Option<String> {
        if self.v.ndev == 0 {
            return None;
        }
        let vs = &self.vs;
        Some(format!(
            "{tag}: virtio kicks {}, blk requests {} (reads {}, writes {}, flushes {}), faults {}, \
             writes held in the volatile cache {} (committed by flush {}, still held {}), net rx {} tx {}",
            vs.kicks,
            vs.reqs,
            vs.reads,
            vs.writes,
            vs.flushes,
            self.v.blk.fault_count.wrapping_add(self.v.net.fault_count),
            vs.held,
            vs.committed,
            self.v.blk.n_unsync,
            vs.rx,
            vs.tx
        ))
    }

    /// hvx's HVX_SET_FAULT (serve.c `set_fault`): new policy, counters from 0, cache emptied.
    pub fn set_fault(&mut self, seed: u64, classes: u32, prob: u32) {
        if self.blk_idx < 0 {
            return;
        }
        let b = &mut self.v.blk;
        b.seed = seed;
        b.classes = classes;
        b.prob = prob;
        b.max_delay = 0;
        b.req_seq = 0;
        b.kick_seq = 0;
        b.fault_count = 0;
        self.cache_clear();
    }

    // ---------------- the block device ----------------

    fn decide(&self, idx: u64, ty: u32) -> (Bf, u32) {
        let b = &self.v.blk;
        if b.classes == 0 {
            return (Bf::None, 0);
        }
        let h = fault_roll(b.seed, idx);
        if ((h % 100) as u32) >= b.prob {
            return (Bf::None, 0);
        }
        let mut c = Vec::with_capacity(5);
        if b.classes & F_DELAY != 0 {
            c.push(Bf::Delay);
        }
        if b.classes & F_ERROR != 0 {
            c.push(Bf::Error);
        }
        if b.classes & F_DROP != 0 && ty == T_OUT {
            c.push(Bf::Drop);
        }
        if b.classes & F_TORN != 0 && ty == T_OUT {
            c.push(Bf::Torn);
        }
        if b.classes & F_UNFLUSHED != 0 && ty == T_OUT {
            c.push(Bf::Unflush);
        }
        if c.is_empty() {
            return (Bf::None, 0);
        }
        let s = c[((h >> 8) % c.len() as u64) as usize];
        let mut extra = 0;
        if s == Bf::Delay {
            let md = if b.max_delay != 0 { b.max_delay } else { 1 };
            extra = 1 + ((h >> 16) % md as u64) as u32;
        }
        (s, extra)
    }

    fn blk_features(&self) -> u64 {
        if self.v.blk.classes & F_UNFLUSHED != 0 {
            1 << 9
        } else {
            0
        }
    }

    fn blk_config(&self, off: u64, size: u32) -> u64 {
        let cap = self.v.blk.disk_size / SECTOR;
        match off {
            0 => {
                if size == 8 {
                    cap
                } else {
                    cap as u32 as u64
                }
            }
            4 => (cap >> 32) as u32 as u64,
            _ => 0,
        }
    }

    fn cache_clear(&mut self) {
        self.cache.clear();
        self.v.blk.n_unsync = 0;
    }

    fn disk_range(&self, doff: u64, len: u32) -> Option<std::ops::Range<usize>> {
        let ds = self.v.blk.disk_size;
        if doff > ds || doff.wrapping_add(len as u64) > ds {
            return None;
        }
        Some(doff as usize..(doff + len as u64) as usize)
    }

    /// `disk_rw`: disk -> guest (`to_guest`) or guest -> disk.
    fn disk_rw(&mut self, ram: &mut Ram, gpa: u64, doff: u64, len: u32, to_guest: bool) -> bool {
        let Some(r) = self.disk_range(doff, len) else {
            return false;
        };
        let disk = self.disk.as_mut().unwrap();
        if to_guest {
            ram.write(gpa, &disk[r])
        } else {
            ram.read(gpa, &mut disk[r])
        }
    }

    fn cache_stash(&mut self, ram: &Ram, doff: u64, gpa: u64, len: u32) -> bool {
        if len == 0 || self.disk_range(doff, len).is_none() {
            return false;
        }
        let mut data = vec![0u8; len as usize];
        if !ram.read(gpa, &mut data) {
            return false;
        }
        self.cache.push(Unsync {
            doff,
            len,
            valid: 1,
            data,
        });
        self.v.blk.n_unsync += 1;
        self.vs.held += 1;
        true
    }

    fn cache_overlay(&self, ram: &mut Ram, gpa: u64, doff: u64, len: u32) {
        for c in &self.cache {
            if c.valid == 0 {
                continue;
            }
            let es = c.doff;
            let ee = es + c.len as u64;
            let os = es.max(doff);
            let oe = ee.min(doff + len as u64);
            if os < oe {
                ram.write(
                    gpa + (os - doff),
                    &c.data[(os - es) as usize..(oe - es) as usize],
                );
            }
        }
    }

    fn cache_invalidate(&mut self, doff: u64, len: u32) {
        for c in &mut self.cache {
            if c.valid != 0 && doff < c.doff + c.len as u64 && c.doff < doff + len as u64 {
                c.valid = 0;
            }
        }
    }

    fn cache_commit(&mut self) {
        let disk = self.disk.as_mut().unwrap();
        for c in &self.cache {
            if c.valid != 0 {
                disk[c.doff as usize..c.doff as usize + c.len as usize].copy_from_slice(&c.data);
                self.vs.committed += 1;
            }
        }
        self.cache_clear();
    }

    fn blk_notify(&mut self, ram: &mut Ram, di_dev: usize, now: u64) {
        let q = self.v.dev[di_dev].q[0];
        if q.ready == 0 || q.num == 0 {
            return;
        }
        let Some(avail_idx) = ram.r16(q.avail + 2) else {
            return;
        };
        let mut any = false;
        let do_reorder = self.v.blk.classes & F_REORDER != 0;
        let mut kick_extra: u64 = 0;
        let mut pend: Vec<(u32, u32)> = Vec::new();
        let mut last_avail = q.last_avail;
        while last_avail as u16 != avail_idx {
            let slot = (last_avail % q.num) as u16;
            let Some(head) = ram.r16(q.avail + 4 + 2 * slot as u64) else {
                break;
            };
            let (mut ty, mut used_len) = (0u32, 0u32);
            let (mut sector, mut doff, mut status_gpa) = (0u64, 0u64, 0u64);
            let mut status = S_OK;
            let mut have_status = false;
            let mut first = true;
            let mut fc = Bf::None;
            let mut di = head;
            let mut g = 0u32;
            while g < q.num.wrapping_add(2) {
                g += 1;
                let Some((addr, len, flags, next)) = ram.desc(q.desc + di as u64 * 16) else {
                    break;
                };
                let has_next = flags & VQ_F_NEXT != 0;
                if first {
                    let mut hdr = [0u8; 16];
                    if ram.read(addr, &mut hdr) {
                        ty = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
                        sector = u64::from_le_bytes(hdr[8..16].try_into().unwrap());
                        doff = sector.wrapping_mul(SECTOR);
                    }
                    self.vs.reqs += 1;
                    match ty {
                        T_IN => self.vs.reads += 1,
                        T_OUT => self.vs.writes += 1,
                        T_FLUSH => self.vs.flushes += 1,
                        _ => {}
                    }
                    if self.v.blk.classes != 0 {
                        let seq = self.v.blk.req_seq;
                        self.v.blk.req_seq += 1;
                        let (f, extra) = self.decide(seq, ty);
                        fc = f;
                        if fc != Bf::None {
                            self.v.blk.fault_count += 1;
                        }
                        if fc == Bf::Error {
                            status = S_IOERR;
                        } else if fc == Bf::Delay {
                            kick_extra += extra as u64;
                        }
                    }
                    if ty == T_FLUSH && fc != Bf::Error {
                        self.cache_commit();
                    } else if ty != T_IN && ty != T_OUT && ty != T_FLUSH && status == S_OK {
                        status = S_UNSUPP;
                    }
                } else if !has_next {
                    status_gpa = addr;
                    have_status = true;
                } else if ty == T_IN || ty == T_OUT {
                    let mut ok = true;
                    if fc == Bf::Error {
                        status = S_IOERR;
                    } else if ty == T_OUT && fc == Bf::Drop {
                        self.cache_invalidate(doff, len);
                    } else if ty == T_OUT && fc == Bf::Unflush {
                        if !self.cache_stash(ram, doff, addr, len) {
                            ok = self.disk_rw(ram, addr, doff, len, false);
                            self.cache_invalidate(doff, len);
                        }
                    } else if ty == T_OUT && fc == Bf::Torn {
                        let torn_end = sector.wrapping_mul(SECTOR).wrapping_add(SECTOR);
                        if doff < torn_end {
                            let n = if doff.wrapping_add(len as u64) <= torn_end {
                                len
                            } else {
                                (torn_end - doff) as u32
                            };
                            ok = self.disk_rw(ram, addr, doff, n, false);
                        }
                    } else if ty == T_IN {
                        ok = self.disk_rw(ram, addr, doff, len, true);
                        if ok {
                            self.cache_overlay(ram, addr, doff, len);
                        }
                        used_len = used_len.wrapping_add(len);
                    } else {
                        ok = self.disk_rw(ram, addr, doff, len, false);
                        self.cache_invalidate(doff, len);
                    }
                    if !ok {
                        status = S_IOERR;
                    }
                    doff = doff.wrapping_add(len as u64);
                }
                first = false;
                if !has_next {
                    break;
                }
                di = next;
            }
            if have_status {
                ram.write(status_gpa, &[status]);
                used_len = used_len.wrapping_add(1);
            }
            if do_reorder && pend.len() < REORDER_MAX {
                pend.push((head as u32, used_len));
            } else if let Some(ui) = ram.r16(q.used + 2) {
                ram.used(&q, ui, head as u32, used_len);
                ram.w16(q.used + 2, ui.wrapping_add(1));
            }
            last_avail = (last_avail as u16).wrapping_add(1) as u32;
            self.v.dev[di_dev].q[0].last_avail = last_avail;
            any = true;
        }
        if !pend.is_empty() {
            let b = &self.v.blk;
            let mut pr = mix((b.seed ^ 0xD15E_A5E5_C0FF_EE11)
                .wrapping_add((b.kick_seq.wrapping_add(1)).wrapping_mul(GOLDEN)));
            let n = pend.len();
            let mut order: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                pr = mix(pr);
                let j = (pr % (i as u64 + 1)) as usize;
                order.swap(i, j);
            }
            if let Some(mut ui) = ram.r16(q.used + 2) {
                for &o in &order {
                    ram.used(&q, ui, pend[o].0, pend[o].1);
                    ui = ui.wrapping_add(1);
                }
                ram.w16(q.used + 2, ui);
            }
        }
        if any {
            self.complete(di_dev, now, kick_extra);
            if self.v.blk.classes != 0 {
                self.v.blk.kick_seq += 1;
            }
        }
    }

    // ---------------- the network device ----------------

    fn net_config(&self, off: u64, size: u32) -> u64 {
        let mut v = 0u64;
        let mut k = 0;
        while k < size && k < 8 {
            let o = off + k as u64;
            let b: u64 = if o < 6 {
                self.v.net.mac[o as usize] as u64
            } else if o == 8 {
                1
            } else {
                0
            };
            v |= b << (8 * k);
            k += 1;
        }
        v
    }

    fn ndecide(&self, tx: bool, idx: u64) -> (Nf, u32, u8) {
        let n = &self.v.net;
        if n.classes == 0 {
            return (Nf::None, 0, 0);
        }
        let h = mix((n.seed ^ if tx { TX_SALT } else { RX_SALT })
            .wrapping_add(idx.wrapping_add(1).wrapping_mul(GOLDEN)));
        if ((h % 100) as u32) >= n.prob {
            return (Nf::None, 0, 0);
        }
        let mut c = Vec::with_capacity(3);
        if n.classes & NF_DROP != 0 {
            c.push(Nf::Drop);
        }
        if n.classes & NF_CORRUPT != 0 {
            c.push(Nf::Corrupt);
        }
        if n.classes & NF_DUP != 0 {
            c.push(Nf::Dup);
        }
        if c.is_empty() {
            return (Nf::None, 0, 0);
        }
        let s = c[((h >> 8) % c.len() as u64) as usize];
        if s == Nf::Corrupt {
            return (s, (h >> 16) as u32, ((h >> 32) | 1) as u8);
        }
        (s, 0, 0)
    }

    fn rx_one(&mut self, ram: &mut Ram, i: usize, frame: &[u8]) -> bool {
        let q = self.v.dev[i].q[0];
        if q.ready == 0 || q.num == 0 {
            return false;
        }
        let Some(avail_idx) = ram.r16(q.avail + 2) else {
            return false;
        };
        if q.last_avail as u16 == avail_idx {
            return false;
        }
        let Some(head) = ram.r16(q.avail + 4 + 2u32.wrapping_mul(q.last_avail % q.num) as u64)
        else {
            return false;
        };
        let Some((addr, len, flags, _)) = ram.desc(q.desc + head as u64 * 16) else {
            return false;
        };
        let mut hdr = [0u8; HDR_LEN as usize];
        hdr[10] = 1; // num_buffers
        let flen = frame.len() as u32;
        let mut used = 0;
        if flags & VQ_F_WRITE != 0 && len >= HDR_LEN + flen {
            ram.write(addr, &hdr);
            ram.write(addr + HDR_LEN as u64, frame);
            used = HDR_LEN + flen;
        }
        if let Some(ui) = ram.r16(q.used + 2) {
            ram.used(&q, ui, head as u32, used);
            ram.w16(q.used + 2, ui.wrapping_add(1));
        }
        self.v.dev[i].q[0].last_avail = (q.last_avail as u16).wrapping_add(1) as u32;
        true
    }

    fn net_poll(&mut self, ram: &mut Ram, now: u64) {
        let i = self.net_idx as usize;
        let n = self.v.net;
        let q = self.v.dev[i].q[0];
        if n.rx_head >= n.rx_n || now < n.rx_next_at || q.ready == 0 {
            return;
        }
        match ram.r16(q.avail + 2) {
            Some(a) if q.last_avail as u16 != a => {}
            _ => return,
        }
        let mut f = self.net_rx[n.rx_head as usize].clone();
        self.v.net.rx_head += 1;
        self.v.net.rx_next_at = now.wrapping_add(n.pace);
        let mut fc = Nf::None;
        let (mut cpos, mut cx) = (0u32, 0u8);
        if n.classes != 0 {
            let seq = self.v.net.rx_seq;
            self.v.net.rx_seq += 1;
            (fc, cpos, cx) = self.ndecide(false, seq);
            if fc != Nf::None {
                self.v.net.fault_count += 1;
            }
        }
        if fc == Nf::Drop {
            return;
        }
        if fc == Nf::Corrupt && !f.is_empty() {
            let l = f.len();
            f[cpos as usize % l] ^= cx;
        }
        let mut got = self.rx_one(ram, i, &f);
        if fc == Nf::Dup {
            got |= self.rx_one(ram, i, &f);
        }
        if got {
            self.vs.rx += 1;
            self.complete(i, now, 0);
        }
    }

    fn net_next_due(&self, ram: &Ram, _now: u64) -> u64 {
        let i = self.net_idx as usize;
        let q = self.v.dev[i].q[0];
        if self.v.net.rx_head >= self.v.net.rx_n || q.ready == 0 {
            return 0;
        }
        match ram.r16(q.avail + 2) {
            Some(a) if q.last_avail as u16 != a => self.v.net.rx_next_at,
            _ => 0,
        }
    }

    fn tx_capture(&mut self, f: &[u8]) {
        let n = f.len() as u64;
        let at = self.v.net.tx_len;
        if at + 2 + n > NET_TX_CAP as u64 {
            return;
        }
        let t = self.net_tx.as_mut().unwrap();
        let at = at as usize;
        t[at] = (n >> 8) as u8;
        t[at + 1] = n as u8;
        t[at + 2..at + 2 + f.len()].copy_from_slice(f);
        self.v.net.tx_len += 2 + n;
        self.vs.tx += 1;
    }

    fn net_notify(&mut self, ram: &mut Ram, i: usize, qi: i32, now: u64) {
        if qi != 1 {
            return; // receive-ring kicks: delivery is paced (net_poll)
        }
        let q = self.v.dev[i].q[1];
        if q.ready == 0 || q.num == 0 {
            return;
        }
        let Some(avail_idx) = ram.r16(q.avail + 2) else {
            return;
        };
        let mut any = false;
        let mut last_avail = q.last_avail;
        while last_avail as u16 != avail_idx {
            let Some(head) = ram.r16(q.avail + 4 + 2u32.wrapping_mul(last_avail % q.num) as u64)
            else {
                break;
            };
            let mut f = [0u8; NET_FRAME_MAX];
            let (mut n, mut skip) = (0u32, HDR_LEN);
            let mut di = head;
            let mut g = 0u32;
            while g < q.num.wrapping_add(2) {
                g += 1;
                let Some((addr, len, flags, next)) = ram.desc(q.desc + di as u64 * 16) else {
                    break;
                };
                if flags & VQ_F_WRITE == 0 {
                    let (mut off, mut len) = (0u32, len);
                    if skip != 0 {
                        let s = skip.min(len);
                        skip -= s;
                        off += s;
                        len -= s;
                    }
                    if len != 0 && n + len <= NET_FRAME_MAX as u32 {
                        ram.read(addr + off as u64, &mut f[n as usize..(n + len) as usize]);
                        n += len;
                    }
                }
                if flags & VQ_F_NEXT == 0 {
                    break;
                }
                di = next;
            }
            let mut fc = Nf::None;
            let (mut cpos, mut cx) = (0u32, 0u8);
            if self.v.net.classes != 0 {
                let seq = self.v.net.tx_seq;
                self.v.net.tx_seq += 1;
                (fc, cpos, cx) = self.ndecide(true, seq);
                if fc != Nf::None {
                    self.v.net.fault_count += 1;
                }
            }
            if fc == Nf::Corrupt && n != 0 {
                f[(cpos % n) as usize] ^= cx;
            }
            if fc != Nf::Drop {
                self.tx_capture(&f[..n as usize]);
                if fc == Nf::Dup {
                    self.tx_capture(&f[..n as usize]);
                }
            }
            if let Some(ui) = ram.r16(q.used + 2) {
                ram.used(&q, ui, head as u32, 0);
                ram.w16(q.used + 2, ui.wrapping_add(1));
            }
            last_avail = (last_avail as u16).wrapping_add(1) as u32;
            self.v.dev[i].q[1].last_avail = last_avail;
            any = true;
        }
        if any {
            self.complete(i, now, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vstate_layout() {
        assert_eq!(VState::default().bytes().len(), VSTATE_SIZE);
        assert_eq!(parse_classes(Some("unflushed"), false), F_UNFLUSHED);
        assert_eq!(parse_classes(Some("lying,drop"), false), F_DROP);
        assert_eq!(parse_classes(None, true), NF_DROP);
    }
}
