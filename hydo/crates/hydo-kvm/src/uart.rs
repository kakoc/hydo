//! The console: a 16550 on I/O ports 0x3f8..0x3ff and IRQ 4, the Mac engine's UART model
//! (hydo-hvm's `vmm`, from hv/hvm/hvm.c) with port I/O instead of MMIO: the transmitter is always
//! empty (THRE), so with its interrupt enabled the line is up until the guest disables it; the
//! receiver is the host's typed input (the lock-step SEND), the line is up while a byte waits and
//! its interrupt is enabled; the modem loopback the 8250 driver probes reflects MCR.

pub const BASE: u16 = 0x3f8;
pub const IRQ: u8 = 4;
/// The receive queue
pub const RXQ: usize = 4096;

/// The registers (in the snapshot).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Regs {
    pub ier: u8,
    pub lcr: u8,
    pub mcr: u8,
    pub scr: u8,
    pub dll: u8,
    pub dlm: u8,
    pub fcr: u8,
}

impl Regs {
    pub fn bytes(&self) -> [u8; 7] {
        [
            self.ier, self.lcr, self.mcr, self.scr, self.dll, self.dlm, self.fcr,
        ]
    }
}

/// The receive queue: input typed by the host, not in any snapshot (as on the Mac: a SNAP is
/// refused while a typed send is unacknowledged, and RESUME types it again).
pub struct Rx {
    q: Box<[u8; RXQ]>,
    pub head: u32,
    pub tail: u32,
}

impl Default for Rx {
    fn default() -> Rx {
        Rx {
            q: Box::new([0u8; RXQ]),
            head: 0,
            tail: 0,
        }
    }
}

impl Rx {
    pub fn ready(&self) -> bool {
        self.head != self.tail
    }
    pub fn room(&self) -> u32 {
        (RXQ as u32).wrapping_sub(self.tail.wrapping_sub(self.head))
    }
    pub fn push(&mut self, c: u8) {
        self.q[self.tail as usize % RXQ] = c;
        self.tail = self.tail.wrapping_add(1);
    }
    fn pop(&mut self) -> u8 {
        let c = self.q[self.head as usize % RXQ];
        self.head = self.head.wrapping_add(1);
        c
    }
    pub fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
    }
}

/// The interrupt line.
pub fn level(u: &Regs, rx: &Rx) -> bool {
    (u.ier & 0x02) != 0 || ((u.ier & 0x01) != 0 && rx.ready())
}

/// A read of register `off` (0..7).
pub fn read(u: &Regs, rx: &mut Rx, off: u16) -> u8 {
    let dlab = u.lcr & 0x80 != 0;
    match off {
        0 => {
            if dlab {
                u.dll
            } else if rx.ready() {
                rx.pop()
            } else {
                0
            }
        }
        1 => {
            if dlab {
                u.dlm
            } else {
                u.ier
            }
        }
        2 => {
            let fifo = if u.fcr & 1 != 0 { 0xc0 } else { 0 };
            if u.ier & 0x01 != 0 && rx.ready() {
                return fifo | 0x04;
            }
            fifo | if u.ier & 0x02 != 0 { 0x02 } else { 0x01 }
        }
        3 => u.lcr,
        4 => u.mcr,
        5 => 0x60 | if rx.ready() { 0x01 } else { 0 },
        6 => {
            if u.mcr & 0x10 != 0 {
                let m = u.mcr;
                ((m & 2) << 3) | ((m & 1) << 5) | ((m & 4) << 4) | ((m & 8) << 4)
            } else {
                0xb0
            }
        }
        7 => u.scr,
        _ => 0,
    }
}

/// A write of register `off`: Some(byte) when it is a character for the console.
pub fn write(u: &mut Regs, off: u16, v: u8) -> Option<u8> {
    let dlab = u.lcr & 0x80 != 0;
    match off {
        0 => {
            if dlab {
                u.dll = v;
                None
            } else {
                Some(v)
            }
        }
        1 => {
            if dlab {
                u.dlm = v
            } else {
                u.ier = v & 0x0f
            }
            None
        }
        2 => {
            u.fcr = v;
            None
        }
        3 => {
            u.lcr = v;
            None
        }
        4 => {
            u.mcr = v;
            None
        }
        7 => {
            u.scr = v;
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_rx_and_the_line() {
        let mut u = Regs::default();
        let mut rx = Rx::default();
        assert_eq!(write(&mut u, 0, b'A'), Some(b'A'));
        assert!(!level(&u, &rx));
        write(&mut u, 1, 0x02);
        assert!(level(&u, &rx), "THRE interrupt while enabled");
        assert_eq!(read(&u, &mut rx, 2), 0x02);
        write(&mut u, 1, 0x01);
        assert!(!level(&u, &rx));
        rx.push(b'x');
        assert!(level(&u, &rx));
        assert_eq!(read(&u, &mut rx, 5), 0x61);
        assert_eq!(read(&u, &mut rx, 2), 0x04);
        assert_eq!(read(&u, &mut rx, 0), b'x');
        assert!(!level(&u, &rx));
        // DLAB
        write(&mut u, 3, 0x80);
        assert_eq!(write(&mut u, 0, 3), None);
        assert_eq!(read(&u, &mut rx, 0), 3);
        // the 8250 driver's 16550A test: FIFO bits in IIR
        write(&mut u, 3, 0x03);
        write(&mut u, 2, 0x01);
        assert_eq!(read(&u, &mut rx, 2) >> 6, 3);
        // loopback probe: MCR 0x1a reads back as MSR 0x90 (CTS from RTS, DCD from OUT2)
        write(&mut u, 4, 0x1a);
        assert_eq!(read(&u, &mut rx, 6) & 0xf0, 0x90);
    }
}
