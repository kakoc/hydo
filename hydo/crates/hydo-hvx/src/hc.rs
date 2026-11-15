//! The host-call port's input (M138, M261): what the engine stages for the agent so that the
//! agent reads every line whole. Pure, so that it is tested on every system; the engine
//! (`engine.rs`, Linux only) reads the module's state and puts the bytes.

use crate::abi::{hvx_regs, HVX_HC_AVAIL_PORT};

/// M261: how many newline bytes the engine stages before a line on the host-call port, so that
/// the agent reads the line whole.
///
/// The agent (`hydo-agent`'s `hc.rs`) reads the count of staged bytes (`HVX_HC_AVAIL_PORT`) and
/// then that many bytes with 4-byte INs of the data port, and the module's 4-byte IN pops up to 4
/// bytes. When the count is not a multiple of 4, its last IN pops 1 to 3 bytes past it. Before
/// M261 nothing was there, unless the host put a line while the agent was inside that read (a
/// stop can come at any exit of it). Then the last IN took the new line's first bytes, the agent
/// dropped them, and the line reached it without its `{`, `{"` or `{"i`: in the SWUpdate example
/// of Milestone 259, `ERROR agent protocol: the agent refused a line it could not read`. Padding
/// the unread bytes to a multiple of 4 gives that last IN newlines. An agent that had not started
/// its read sees them as empty lines, which it skips.
///
/// `unread`: the staged bytes the guest has not read; `staged`: the unread bytes right after the
/// engine's previous put; `count_read`: a vCPU has just read the count (`hc_count_just_read`).
/// The guest is inside its read when it took some of the bytes since the put (`unread <
/// staged`), or when it took none but read the count. Otherwise nothing is padded, so a run
/// whose puts never fell inside the agent's read keeps its bytes and its fingerprint.
pub fn hc_pad(unread: u32, staged: u32, count_read: bool) -> usize {
    let inside = unread < staged || (unread == staged && count_read);
    if unread == 0 || unread.is_multiple_of(4) || !inside {
        return 0;
    }
    (4 - unread % 4) as usize
}

/// M261: whether a vCPU stands right after the agent's read of the staged count: an IN of
/// `HVX_HC_AVAIL_PORT` leaves the port in DX and the count in EAX, and the agent's next exit is
/// the first IN of the data port.
pub fn hc_count_just_read(r: &hvx_regs, unread: u32) -> bool {
    r.rdx & 0xffff == u64::from(HVX_HC_AVAIL_PORT) && r.rax as u32 == unread
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module's host-call input (`hvx_hc_pio` and HVX_HC_PUT in `hv/hvx/hvx.c`): staged
    /// bytes, the read offset, and a 4-byte IN that pops up to 4 of them.
    #[derive(Default)]
    struct Port {
        buf: Vec<u8>,
        off: usize,
    }

    impl Port {
        fn put(&mut self, b: &[u8]) {
            self.buf.drain(..self.off); // HVX_HC_PUT drops what the guest has read
            self.off = 0;
            self.buf.extend_from_slice(b);
        }
        fn avail(&self) -> u32 {
            (self.buf.len() - self.off) as u32
        }
        fn pop4(&mut self) -> [u8; 4] {
            let mut w = [0u8; 4];
            for x in &mut w {
                if self.off < self.buf.len() {
                    *x = self.buf[self.off];
                    self.off += 1;
                }
            }
            if self.off == self.buf.len() {
                self.buf.clear();
                self.off = 0;
            }
            w
        }
    }

    /// The agent's `read_all` (`hydo-agent/src/hc.rs`), one IN at a time: `left` is Some while
    /// it is inside a read of the count it took.
    #[derive(Default)]
    struct Agent {
        left: Option<usize>,
        got: Vec<u8>,
        /// The vCPU's DX and EAX after the agent's last IN.
        regs: crate::abi::hvx_regs,
    }

    impl Agent {
        /// One IN; false when the read is over (the count was 0).
        fn step(&mut self, p: &mut Port) -> bool {
            match self.left {
                None | Some(0) => {
                    let n = p.avail();
                    self.regs.rdx = u64::from(crate::abi::HVX_HC_AVAIL_PORT);
                    self.regs.rax = u64::from(n);
                    self.left = (n > 0).then_some(n as usize);
                    n > 0
                }
                Some(l) => {
                    let w = p.pop4();
                    self.regs.rdx = u64::from(crate::abi::HVX_HC_DATA_PORT);
                    self.regs.rax = u64::from(u32::from_le_bytes(w));
                    let k = l.min(4);
                    self.got.extend_from_slice(&w[..k]);
                    self.left = Some(l - k);
                    true
                }
            }
        }
        fn lines(&self) -> Vec<Vec<u8>> {
            self.got
                .split(|&c| c == b'\n')
                .filter(|l| !l.is_empty())
                .map(<[u8]>::to_vec)
                .collect()
        }
    }

    /// The engine's put (`ctl_type` with `hc_pad_now`), against the model.
    fn put(p: &mut Port, staged: &mut u32, a: &Agent, line: &[u8]) -> usize {
        let unread = p.avail();
        let count_read = hc_count_just_read(&a.regs, unread) && a.left == Some(unread as usize);
        let pad = hc_pad(unread, *staged, count_read);
        let mut b = vec![b'\n'; pad];
        b.extend_from_slice(line);
        p.put(&b);
        *staged = unread + b.len() as u32;
        pad
    }

    fn line(n: usize, c: u8) -> Vec<u8> {
        let mut l = b"{\"id\":".to_vec();
        l.resize(n - 1, c);
        l.push(b'\n');
        l
    }

    /// M261 (the SWUpdate example of Milestone 259): the host puts a line while the agent is
    /// inside its read of the line before. Without the padding the agent's last 4-byte IN took
    /// the new line's first bytes (`ERROR agent protocol: the agent refused a line it could not
    /// read: bad-json ...`); with it, both lines arrive whole, at every point of the read and for
    /// every length of the first line.
    #[test]
    fn a_line_put_inside_the_agents_read_reaches_it_whole() {
        let second = line(258, b'b');
        for n in 8..=40 {
            let first = line(n, b'a');
            // the agent's INs before the second put: 0 = it has not started; 1 = it read the
            // count; more = it popped some of the bytes too
            for ins in 0..=n.div_ceil(4) + 2 {
                let (mut p, mut a, mut staged) = (Port::default(), Agent::default(), 0u32);
                assert_eq!(put(&mut p, &mut staged, &a, &first), 0);
                for _ in 0..ins {
                    a.step(&mut p);
                }
                let pad = put(&mut p, &mut staged, &a, &second);
                while a.step(&mut p) {}
                assert_eq!(
                    a.lines(),
                    vec![first[..n - 1].to_vec(), second[..257].to_vec()],
                    "first line of {n} bytes, second put after {ins} INs (pad {pad})"
                );
                // nothing is padded unless the agent was inside its read with a count that
                // is not a multiple of 4: other runs keep their bytes
                if ins == 0 || ins > n.div_ceil(4) || n % 4 == 0 {
                    assert_eq!(pad, 0, "first line of {n} bytes, {ins} INs");
                }
            }
        }
    }

    /// The same read without M261's padding loses the second line's first bytes: the cause.
    #[test]
    fn without_the_padding_the_agents_last_read_takes_the_next_lines_first_bytes() {
        let (first, second) = (line(482, b'a'), line(258, b'b'));
        let (mut p, mut a) = (Port::default(), Agent::default());
        p.put(&first);
        for _ in 0..11 {
            a.step(&mut p); // the count and 40 bytes, as in the example's run
        }
        assert_eq!(p.avail(), 442);
        p.put(&second);
        while a.step(&mut p) {}
        let lines = a.lines();
        assert_eq!(lines[0], first[..481].to_vec());
        assert_eq!(
            lines[1],
            second[2..257].to_vec(),
            "the line lost its `{{\"`"
        );
    }
}
