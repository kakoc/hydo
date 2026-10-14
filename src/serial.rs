//! Minimal 16550 UART at port base 0x3f8, output-only enough for a console.
//!
//! The guest writes bytes to the transmit-holding register (base+0) and may poll
//! the line-status register (base+5) to check that the transmitter is empty. We
//! report "always empty" so a polling guest never blocks. All output is captured
//! into a buffer so it is part of the deterministic, hashable machine state.

pub const COM1_BASE: u16 = 0x3f8;
const THR: u16 = COM1_BASE; // transmit holding / receive buffer
const LSR: u16 = COM1_BASE + 5; // line status register

pub struct Serial {
    /// Everything the guest has printed, in order. Deterministic output stream.
    pub out: Vec<u8>,
    /// Interrupt-enable and other latch registers we accept-and-ignore, kept so
    /// reads return what was written (harmless, keeps well-behaved drivers happy).
    scratch: u8,
}

impl Serial {
    pub fn new() -> Self {
        Serial {
            out: Vec::new(),
            scratch: 0,
        }
    }

    /// Handle an OUT to a COM1 port.
    pub fn io_out(&mut self, port: u16, data: &[u8]) {
        if port == THR {
            self.out.extend_from_slice(data);
        } else if port == COM1_BASE + 7 {
            self.scratch = data.first().copied().unwrap_or(0);
        }
        // Other registers (IER, FCR, LCR, MCR, ...) are accepted and ignored.
    }

    /// Handle an IN from a COM1 port, filling `data`.
    pub fn io_in(&mut self, port: u16, data: &mut [u8]) {
        let val = match port {
            // Line status: THR empty (0x20) + transmitter empty (0x40) always set.
            LSR => 0x60,
            p if p == COM1_BASE + 7 => self.scratch,
            _ => 0,
        };
        if let Some(b) = data.first_mut() {
            *b = val;
        }
        for b in data.iter_mut().skip(1) {
            *b = 0;
        }
    }
}
