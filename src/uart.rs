//! A more complete 16550A UART for the Linux boot path. The kernel's 8250 driver
//! probes registers (scratch test, FIFO, divisor latch) and drives the line-status
//! and interrupt-identification registers, so — unlike the output-only console in
//! `serial.rs` used by the determinism engine — this models the full register set.
//!
//! Output bytes are streamed to a sink immediately (so we can watch the boot log
//! live). A receive queue plus the interrupt-pending computation let the guest
//! take keyboard input over ttyS0 when input is wired up.

use std::collections::VecDeque;

pub const COM1_BASE: u16 = 0x3f8;
pub const COM1_IRQ: u32 = 4;

// Register offsets from the base (meaning depends on DLAB for 0 and 1).
const REG_DATA: u16 = 0; // RBR/THR, or DLL when DLAB=1
const REG_IER: u16 = 1; // interrupt enable, or DLM when DLAB=1
const REG_IIR_FCR: u16 = 2; // read: IIR; write: FCR
const REG_LCR: u16 = 3;
const REG_MCR: u16 = 4;
const REG_LSR: u16 = 5;
const REG_MSR: u16 = 6;
const REG_SCR: u16 = 7;

// IER bits.
const IER_RX: u8 = 1 << 0; // received-data-available interrupt
const IER_THR: u8 = 1 << 1; // transmitter-holding-register-empty interrupt

// LSR bits.
const LSR_DR: u8 = 1 << 0; // data ready
const LSR_THRE: u8 = 1 << 5; // transmit holding register empty
const LSR_TEMT: u8 = 1 << 6; // transmitter empty

// LCR bit 7 selects the divisor latch.
const LCR_DLAB: u8 = 1 << 7;

pub struct Uart {
    ier: u8,
    fcr: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
    rx: VecDeque<u8>,
    /// Everything transmitted, in order (kept so the boot output is inspectable).
    pub out: Vec<u8>,
    /// The IRQ level we last drove, so callers only re-issue KVM_IRQ_LINE on change.
    pub irq_level: bool,
}

impl Uart {
    pub fn new() -> Self {
        Uart {
            ier: 0,
            fcr: 0,
            lcr: 0,
            mcr: 0,
            scr: 0,
            dll: 0,
            dlm: 0,
            rx: VecDeque::new(),
            out: Vec::new(),
            irq_level: false,
        }
    }

    pub fn handles(port: u16) -> bool {
        (COM1_BASE..COM1_BASE + 8).contains(&port)
    }

    fn dlab(&self) -> bool {
        self.lcr & LCR_DLAB != 0
    }

    /// Feed a received byte (from stdin) into the RX queue.
    pub fn push_rx(&mut self, b: u8) {
        self.rx.push_back(b);
    }

    /// Handle an OUT.
    pub fn io_out(&mut self, port: u16, data: &[u8]) {
        let val = data.first().copied().unwrap_or(0);
        match port - COM1_BASE {
            REG_DATA => {
                if self.dlab() {
                    self.dll = val;
                } else {
                    // A string OUT (rep outsb) delivers several console bytes.
                    self.out.extend_from_slice(data);
                }
            }
            REG_IER => {
                if self.dlab() {
                    self.dlm = val;
                } else {
                    self.ier = val;
                }
            }
            REG_IIR_FCR => self.fcr = val,
            REG_LCR => self.lcr = val,
            REG_MCR => self.mcr = val,
            // LSR/MSR are read-only; SCR is a scratch register.
            REG_SCR => self.scr = val,
            _ => {}
        }
    }

    /// Handle an IN, filling `data`.
    pub fn io_in(&mut self, port: u16, data: &mut [u8]) {
        let val = match port - COM1_BASE {
            REG_DATA => {
                if self.dlab() {
                    self.dll
                } else {
                    self.rx.pop_front().unwrap_or(0)
                }
            }
            REG_IER => {
                if self.dlab() {
                    self.dlm
                } else {
                    self.ier
                }
            }
            REG_IIR_FCR => self.iir(),
            REG_LCR => self.lcr,
            REG_MCR => self.mcr,
            REG_LSR => self.lsr(),
            // Modem status: carrier detect + data-set-ready + clear-to-send.
            REG_MSR => 0xb0,
            REG_SCR => self.scr,
            _ => 0,
        };
        if let Some(b) = data.first_mut() {
            *b = val;
        }
        for b in data.iter_mut().skip(1) {
            *b = 0;
        }
    }

    fn lsr(&self) -> u8 {
        let mut v = LSR_THRE | LSR_TEMT;
        if !self.rx.is_empty() {
            v |= LSR_DR;
        }
        v
    }

    /// Interrupt identification register. Reflects the highest-priority pending,
    /// enabled interrupt; bit 0 set means "none pending".
    fn iir(&self) -> u8 {
        let fifo = if self.fcr & 0x01 != 0 { 0xc0 } else { 0x00 };
        if self.ier & IER_RX != 0 && !self.rx.is_empty() {
            0x04 | fifo // received data available
        } else if self.ier & IER_THR != 0 {
            0x02 | fifo // THR empty (we transmit instantly, so always empty)
        } else {
            0x01 | fifo // no interrupt pending
        }
    }

    /// The level the COM1 IRQ line should be driven to right now.
    pub fn irq_pending(&self) -> bool {
        (self.ier & IER_RX != 0 && !self.rx.is_empty()) || (self.ier & IER_THR != 0)
    }
}
