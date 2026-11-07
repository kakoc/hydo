//! The interrupt controller: an 8259A pair (master at 0x20/0x21, slave at 0xa0/0xa1, cascaded on
//! the master's line 2), emulated by the engine. The guest's CPUID hides the local APIC, so Linux
//! keeps the PIC ("APIC disabled by BIOS"), and KVM has no in-kernel interrupt controller: every
//! interrupt the guest takes is one the engine chose to inject (`KVM_INTERRUPT`) at a VM exit.
//!
//! Every line is level-triggered: a line's IRR bit is its device's level, recomputed at every
//! exit (as the Mac engine's GIC pending bits are), so a device that keeps its line up after an
//! EOI interrupts again, and one that drops it before the acknowledge is not delivered. The
//! priorities are fixed (line 0 highest); the commands Linux's i8259 driver uses are emulated
//! (ICW1-4, the mask, non-specific and specific EOI, reading IRR or ISR, poll); rotation and the
//! special mask mode are accepted and have no effect. Auto-EOI (ICW4) is honoured.

/// One 8259A.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Chip {
    pub imr: u8,
    pub irr: u8,
    pub isr: u8,
    /// ICW2: the vector of line 0
    pub base: u8,
    /// 0: initialized; 2, 3, 4: the ICW the data port expects next
    pub step: u8,
    pub icw4: u8,
    pub single: u8,
    pub aeoi: u8,
    /// OCW3: a read of the command port returns ISR (else IRR)
    pub read_isr: u8,
    /// OCW3: the next read of the command port is a poll
    pub poll: u8,
}

impl Chip {
    /// The highest-priority line that is requested, not masked, and of higher priority than every
    /// line in service.
    fn best(&self) -> Option<u8> {
        let req = self.irr & !self.imr;
        for n in 0..8u8 {
            if self.isr & (1 << n) != 0 {
                return None; // a line of higher or equal priority is in service
            }
            if req & (1 << n) != 0 {
                return Some(n);
            }
        }
        None
    }

    fn eoi_highest(&mut self) {
        for n in 0..8 {
            if self.isr & (1 << n) != 0 {
                self.isr &= !(1 << n);
                return;
            }
        }
    }

    fn write_cmd(&mut self, v: u8) {
        if v & 0x10 != 0 {
            // ICW1: start the initialization
            self.imr = 0;
            self.isr = 0;
            self.read_isr = 0;
            self.poll = 0;
            self.aeoi = 0;
            self.icw4 = v & 1;
            self.single = (v >> 1) & 1;
            self.step = 2;
        } else if v & 0x08 == 0 {
            // OCW2
            match v >> 5 {
                0b001 | 0b101 => self.eoi_highest(),
                0b011 | 0b111 => self.isr &= !(1 << (v & 7)),
                _ => {} // rotation and priority commands: fixed priorities here
            }
        } else {
            // OCW3
            if v & 0x02 != 0 {
                self.read_isr = v & 1;
            }
            self.poll = (v >> 2) & 1;
        }
    }

    fn write_data(&mut self, v: u8) {
        match self.step {
            2 => {
                self.base = v & 0xf8;
                self.step = if self.single == 0 {
                    3
                } else if self.icw4 != 0 {
                    4
                } else {
                    0
                };
            }
            3 => self.step = if self.icw4 != 0 { 4 } else { 0 },
            4 => {
                self.aeoi = (v >> 1) & 1;
                self.step = 0;
            }
            _ => self.imr = v,
        }
    }

    /// The bytes of the chip's state (for the machine state's hash).
    pub fn bytes(&self) -> [u8; 10] {
        [
            self.imr,
            self.irr,
            self.isr,
            self.base,
            self.step,
            self.icw4,
            self.single,
            self.aeoi,
            self.read_isr,
            self.poll,
        ]
    }
}

/// The pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pic {
    pub m: Chip,
    pub s: Chip,
}

pub const MASTER_CMD: u16 = 0x20;
pub const MASTER_DATA: u16 = 0x21;
pub const SLAVE_CMD: u16 = 0xa0;
pub const SLAVE_DATA: u16 = 0xa1;
const CASCADE: u8 = 2;

impl Pic {
    /// Set the levels of the 16 lines (bit n = line n), as the devices drive them now.
    pub fn set_levels(&mut self, levels: u16) {
        self.s.irr = (levels >> 8) as u8;
        let cascade = self.s.best().is_some() as u8;
        self.m.irr = (levels as u8 & !(1 << CASCADE)) | (cascade << CASCADE);
    }

    /// The vector the CPU would get now, without acknowledging it.
    pub fn pending(&self) -> Option<u8> {
        let n = self.m.best()?;
        if n == CASCADE {
            let k = self.s.best()?;
            return Some(self.s.base.wrapping_add(k));
        }
        Some(self.m.base.wrapping_add(n))
    }

    /// The interrupt acknowledge: the vector and the line (0..15) it is for, now in service.
    pub fn ack(&mut self) -> Option<(u8, u8)> {
        let n = self.m.best()?;
        if self.m.aeoi == 0 {
            self.m.isr |= 1 << n;
        }
        if n == CASCADE {
            let k = self.s.best()?;
            if self.s.aeoi == 0 {
                self.s.isr |= 1 << k;
            }
            return Some((self.s.base.wrapping_add(k), 8 + k));
        }
        Some((self.m.base.wrapping_add(n), n))
    }

    pub fn handles(port: u16) -> bool {
        matches!(port, MASTER_CMD | MASTER_DATA | SLAVE_CMD | SLAVE_DATA)
    }

    pub fn read(&mut self, port: u16) -> u8 {
        let master = port == MASTER_CMD || port == MASTER_DATA;
        if port == MASTER_DATA || port == SLAVE_DATA {
            return if master { self.m.imr } else { self.s.imr };
        }
        let poll = if master { self.m.poll } else { self.s.poll };
        if poll != 0 {
            // the poll command: an acknowledge by a read
            let c = if master { &mut self.m } else { &mut self.s };
            c.poll = 0;
            return match c.best() {
                Some(n) => {
                    if c.aeoi == 0 {
                        c.isr |= 1 << n;
                    }
                    0x80 | n
                }
                None => 0,
            };
        }
        let c = if master { &self.m } else { &self.s };
        if c.read_isr != 0 {
            c.isr
        } else {
            c.irr
        }
    }

    pub fn write(&mut self, port: u16, v: u8) {
        match port {
            MASTER_CMD => self.m.write_cmd(v),
            MASTER_DATA => self.m.write_data(v),
            SLAVE_CMD => self.s.write_cmd(v),
            _ => self.s.write_data(v),
        }
    }

    pub fn bytes(&self) -> [u8; 20] {
        let mut b = [0u8; 20];
        b[..10].copy_from_slice(&self.m.bytes());
        b[10..].copy_from_slice(&self.s.bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linux's init_8259A(0): ICW1-4 on both chips, then the masks.
    fn linux_init(p: &mut Pic) {
        p.write(MASTER_DATA, 0xff);
        p.write(SLAVE_DATA, 0xff);
        p.write(MASTER_CMD, 0x11);
        p.write(MASTER_DATA, 0x30);
        p.write(MASTER_DATA, 1 << CASCADE);
        p.write(MASTER_DATA, 0x01);
        p.write(SLAVE_CMD, 0x11);
        p.write(SLAVE_DATA, 0x38);
        p.write(SLAVE_DATA, CASCADE);
        p.write(SLAVE_DATA, 0x01);
        p.write(MASTER_DATA, 0xfb); // all masked but the cascade
        p.write(SLAVE_DATA, 0xff);
    }

    #[test]
    fn probe_reads_back_the_mask() {
        let mut p = Pic::default();
        p.write(MASTER_DATA, 0xff);
        assert_eq!(p.read(MASTER_DATA), 0xff);
    }

    #[test]
    fn level_delivery_priority_and_eoi() {
        let mut p = Pic::default();
        linux_init(&mut p);
        p.set_levels(1 << 4);
        assert_eq!(p.pending(), None, "masked");
        p.write(MASTER_DATA, 0xfb & !(1 << 4) & !1);
        p.set_levels((1 << 4) | 1);
        assert_eq!(p.pending(), Some(0x30), "line 0 first");
        assert_eq!(p.ack(), Some((0x30, 0)));
        // line 0 in service blocks line 4
        assert_eq!(p.pending(), None);
        // Linux reads ISR through OCW3 to tell a spurious IRQ 7
        p.write(MASTER_CMD, 0x0b);
        assert_eq!(p.read(MASTER_CMD), 1);
        p.write(MASTER_CMD, 0x0a);
        // specific EOI for line 0
        p.write(MASTER_CMD, 0x60);
        p.set_levels(1 << 4);
        assert_eq!(p.pending(), Some(0x34));
        p.ack();
        // the line stays up after the EOI: level-triggered, delivered again
        p.write(MASTER_CMD, 0x20);
        p.set_levels(1 << 4);
        assert_eq!(p.pending(), Some(0x34));
        // the line went down before the acknowledge: nothing
        p.set_levels(0);
        assert_eq!(p.pending(), None);
    }

    #[test]
    fn cascade() {
        let mut p = Pic::default();
        linux_init(&mut p);
        p.write(SLAVE_DATA, !(1 << 2)); // line 10
        p.set_levels(1 << 10);
        assert_eq!(p.pending(), Some(0x3a));
        assert_eq!(p.ack(), Some((0x3a, 10)));
        assert_eq!(p.m.isr, 1 << CASCADE);
        assert_eq!(p.s.isr, 1 << 2);
        p.write(SLAVE_CMD, 0x62);
        p.write(MASTER_CMD, 0x62);
        assert_eq!((p.m.isr, p.s.isr), (0, 0));
    }

    #[test]
    fn auto_eoi_and_poll() {
        let mut p = Pic::default();
        p.write(MASTER_CMD, 0x13); // single, ICW4
        p.write(MASTER_DATA, 0x20);
        p.write(MASTER_DATA, 0x03); // AEOI
        p.write(MASTER_DATA, 0x00);
        p.set_levels(1 << 3);
        assert_eq!(p.ack(), Some((0x23, 3)));
        assert_eq!(p.m.isr, 0);
        p.write(MASTER_CMD, 0x0c);
        assert_eq!(p.read(MASTER_CMD), 0x83);
    }
}
