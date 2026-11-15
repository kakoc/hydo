//! Added disks (Milestone 263, docs/design/devices.md): virtio-mmio block devices after the main
//! devices, each with a volatile write cache of its own. A flush that the guest sends to one of
//! them makes only its own cached writes durable; a power cut drops every added disk's cache. They
//! have no seeded faults. A machine without added disks has the same state and hash as before.

use super::*;

/// An added disk of the configuration: its size in bytes, and whether every write is durable at
/// once (`cache: none`) instead of waiting in the disk's volatile cache until a flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtraCfg {
    pub size: u64,
    pub write_through: bool,
}

impl ExtraCfg {
    /// `<MiB>:<volatile|none>`, the engines' option value of one added disk.
    pub fn parse(s: &str) -> Option<ExtraCfg> {
        let (m, c) = s.split_once(':')?;
        let mib: u64 = m.parse().ok().filter(|m| (1..=4096).contains(m))?;
        let write_through = match c {
            "volatile" => false,
            "none" => true,
            _ => return None,
        };
        Some(ExtraCfg {
            size: mib << 20,
            write_through,
        })
    }

    /// A list of [`ExtraCfg::parse`] values separated by commas (empty: none).
    pub fn parse_list(s: &str) -> Option<Vec<ExtraCfg>> {
        s.split(',')
            .filter(|x| !x.is_empty())
            .map(ExtraCfg::parse)
            .collect()
    }
}

/// One added disk.
#[derive(Clone, Debug)]
pub struct Extra {
    pub dev: VDev,
    pub disk: Disk,
    pub cache: Vec<Unsync>,
    pub size: u64,
    pub write_through: bool,
}

impl Extra {
    fn new(c: &ExtraCfg) -> Extra {
        Extra {
            dev: VDev {
                ty: VDEV_BLK,
                ..VDev::default()
            },
            disk: Disk::from_vec(vec![0u8; c.size as usize]),
            cache: Vec::new(),
            size: c.size,
            write_through: c.write_through,
        }
    }

    fn range(&self, doff: u64, len: u32) -> Option<std::ops::Range<usize>> {
        let end = doff.checked_add(len as u64)?;
        (end <= self.size).then_some(doff as usize..end as usize)
    }

    fn overlay(&self, buf: &mut [u8], doff: u64) {
        let len = buf.len() as u64;
        for c in &self.cache {
            let (es, ee) = (c.doff, c.doff + c.len as u64);
            let (os, oe) = (es.max(doff), ee.min(doff + len));
            if os < oe {
                buf[(os - doff) as usize..(oe - doff) as usize]
                    .copy_from_slice(&c.data[(os - es) as usize..(oe - es) as usize]);
            }
        }
    }

    fn commit(&mut self) {
        for c in std::mem::take(&mut self.cache) {
            self.disk.write(c.doff as usize, &c.data[..c.len as usize]);
        }
    }

    pub(super) fn hash(&self, mut h: u64) -> u64 {
        let d = &self.dev;
        let mut b = B(Vec::new());
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
        b.u32(self.write_through as u32);
        b.u64(self.size);
        b.u64(self.cache.len() as u64);
        h = fnv(&b.0, h);
        self.disk.for_each(self.size as usize, |x| h = fnv(x, h));
        for c in &self.cache {
            h = fnv_u64(c.doff, h);
            h = fnv(&c.len.to_le_bytes(), h);
            h = fnv(&c.data[..c.len as usize], h);
        }
        h
    }
}

impl Virtio {
    /// Add the configured disks after the main devices. Without any the devices are as before.
    pub fn add_extra(&mut self, cfg: &[ExtraCfg]) {
        self.extra = cfg.iter().map(Extra::new).collect();
    }

    /// Every added disk's flushed bytes (what survives a power cut, and what a reboot keeps once
    /// the guest flushed the disk).
    pub fn extra_durable(&self) -> Vec<Disk> {
        self.extra.iter().map(|e| e.disk.clone()).collect()
    }

    /// After a restore from S0: the added disks hold `d` (from [`Virtio::extra_durable`]) and
    /// nothing in their caches.
    pub fn set_extra_durable(&mut self, d: Vec<Disk>) {
        for (e, d) in self.extra.iter_mut().zip(d) {
            e.disk = d;
            e.cache.clear();
        }
    }

    /// The MMIO access of added disk `j` (device `ndev + j`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn extra_mmio(
        &mut self,
        ram: &mut Ram,
        j: usize,
        off: u64,
        write: bool,
        size: u32,
        val: &mut u64,
        now: u64,
    ) {
        let step = self.v.step;
        if off >= 0x100 {
            if !write {
                let cap = self.extra[j].size / SECTOR;
                *val = match off - 0x100 {
                    0 if size == 8 => cap,
                    0 => cap as u32 as u64,
                    4 => (cap >> 32) as u32 as u64,
                    _ => 0,
                };
            }
            return;
        }
        let e = &mut self.extra[j];
        let feats: u64 = (1u64 << 32) | if e.write_through { 0 } else { 1 << 9 };
        let d = &mut e.dev;
        if !write {
            let v: u32 = match off {
                0x000 => 0x7472_6976,
                0x004 => 2,
                0x008 => d.ty,
                0x00c => 0x6f64_7968,
                0x010 => match d.dfsel {
                    0 => feats as u32,
                    1 => (feats >> 32) as u32,
                    _ => 0,
                },
                0x034 if d.qsel < 1 => VQ_SIZE_MAX,
                0x044 => d.q[0].ready,
                0x060 => d.isr_shown,
                0x070 => d.status,
                0x0fc => d.config_gen,
                _ => 0,
            };
            *val = v as u64;
            return;
        }
        let v = *val as u32;
        let q0 = d.qsel < 1;
        match off {
            0x014 => d.dfsel = v,
            0x020 => {
                if d.drfsel < 2 {
                    let sh = 32 * d.drfsel;
                    d.drv_features =
                        (d.drv_features & !(0xffff_ffffu64 << sh)) | ((v as u64) << sh);
                }
            }
            0x024 => d.drfsel = v,
            0x030 => d.qsel = v,
            0x038 if q0 => d.q[0].num = v,
            0x044 if q0 => d.q[0].ready = v & 1,
            0x050 => {
                self.vs.kicks += 1;
                if self.extra_notify(ram, j) {
                    let d = &mut self.extra[j].dev;
                    d.isr |= 1;
                    d.armed = 1;
                    d.due = now.wrapping_add(LATENCY_EXITS.wrapping_mul(step));
                }
            }
            0x064 => d.isr_shown &= !v,
            0x070 => {
                if v == 0 {
                    *d = VDev {
                        ty: VDEV_BLK,
                        ..VDev::default()
                    };
                    e.cache.clear();
                } else {
                    d.status = v;
                }
            }
            0x080 => d.q[0].desc = (d.q[0].desc & !0xffff_ffffu64) | v as u64,
            0x084 => d.q[0].desc = (d.q[0].desc & 0xffff_ffff) | ((v as u64) << 32),
            0x090 => d.q[0].avail = (d.q[0].avail & !0xffff_ffffu64) | v as u64,
            0x094 => d.q[0].avail = (d.q[0].avail & 0xffff_ffff) | ((v as u64) << 32),
            0x0a0 => d.q[0].used = (d.q[0].used & !0xffff_ffffu64) | v as u64,
            0x0a4 => d.q[0].used = (d.q[0].used & 0xffff_ffff) | ((v as u64) << 32),
            _ => {}
        }
    }

    /// The requests of added disk `j`'s queue; whether any was served.
    fn extra_notify(&mut self, ram: &mut Ram, j: usize) -> bool {
        let q = self.extra[j].dev.q[0];
        if q.ready == 0 || q.num == 0 {
            return false;
        }
        let Some(avail_idx) = ram.r16(q.avail + 2) else {
            return false;
        };
        let mut any = false;
        let mut last_avail = q.last_avail;
        while last_avail as u16 != avail_idx {
            let slot = (last_avail % q.num) as u16;
            let Some(head) = ram.r16(q.avail + 4 + 2 * slot as u64) else {
                break;
            };
            let (mut ty, mut used_len, mut doff) = (0u32, 0u32, 0u64);
            let (mut status, mut status_gpa, mut have_status) = (S_OK, 0u64, false);
            let mut first = true;
            let mut di = head;
            let mut g = 0u32;
            while g < q.num.wrapping_add(2) {
                g += 1;
                let Some((addr, len, flags, next)) = ram.desc(q.desc + di as u64 * 16) else {
                    break;
                };
                let has_next = flags & VQ_F_NEXT != 0;
                let e = &mut self.extra[j];
                if first {
                    let mut hdr = [0u8; 16];
                    if ram.read(addr, &mut hdr) {
                        ty = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
                        doff =
                            u64::from_le_bytes(hdr[8..16].try_into().unwrap()).wrapping_mul(SECTOR);
                    }
                    self.vs.reqs += 1;
                    match ty {
                        T_IN => self.vs.reads += 1,
                        T_OUT => self.vs.writes += 1,
                        T_FLUSH => {
                            self.vs.flushes += 1;
                            e.commit();
                        }
                        _ => status = S_UNSUPP,
                    }
                } else if !has_next {
                    status_gpa = addr;
                    have_status = true;
                } else if ty == T_IN || ty == T_OUT {
                    let ok = match e.range(doff, len) {
                        None => false,
                        Some(r) => {
                            let mut buf = vec![0u8; r.len()];
                            if ty == T_IN {
                                e.disk.read(r.start, &mut buf);
                                e.overlay(&mut buf, doff);
                                used_len = used_len.wrapping_add(len);
                                ram.write(addr, &buf)
                            } else if !ram.read(addr, &mut buf) {
                                false
                            } else if e.write_through {
                                e.disk.write(r.start, &buf);
                                true
                            } else {
                                e.cache.push(Unsync {
                                    doff,
                                    len,
                                    valid: 1,
                                    data: buf.into(),
                                });
                                self.vs.held += 1;
                                true
                            }
                        }
                    };
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
            if let Some(ui) = ram.r16(q.used + 2) {
                ram.used(&q, ui, head as u32, used_len);
                ram.w16(q.used + 2, ui.wrapping_add(1));
            }
            last_avail = (last_avail as u16).wrapping_add(1) as u32;
            self.extra[j].dev.q[0].last_avail = last_avail;
            any = true;
        }
        any
    }
}
