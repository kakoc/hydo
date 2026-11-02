//! The flattened device tree hvm.c writes for the guest (`build_fdt`), byte for byte.

use crate::layout::*;

struct W {
    st: Vec<u8>,
    strs: Vec<u8>,
}

impl W {
    fn s32(&mut self, v: u32) {
        self.st.extend_from_slice(&v.to_be_bytes());
    }
    fn sbytes(&mut self, b: &[u8]) {
        self.st.extend_from_slice(b);
        while self.st.len() & 3 != 0 {
            self.st.push(0);
        }
    }
    fn stroff(&mut self, s: &str) -> u32 {
        let mut i = 0;
        while i < self.strs.len() {
            let end = i + self.strs[i..].iter().position(|&c| c == 0).unwrap();
            if &self.strs[i..end] == s.as_bytes() {
                return i as u32;
            }
            i = end + 1;
        }
        let o = self.strs.len() as u32;
        self.strs.extend_from_slice(s.as_bytes());
        self.strs.push(0);
        o
    }
    fn node(&mut self, n: &str) {
        self.s32(1);
        let mut b = n.as_bytes().to_vec();
        b.push(0);
        self.sbytes(&b);
    }
    fn endnode(&mut self) {
        self.s32(2);
    }
    fn prop(&mut self, n: &str, v: &[u8]) {
        self.s32(3);
        self.s32(v.len() as u32);
        let o = self.stroff(n);
        self.s32(o);
        self.sbytes(v);
    }
    fn prop_str(&mut self, n: &str, v: &str) {
        let mut b = v.as_bytes().to_vec();
        b.push(0);
        self.prop(n, &b);
    }
    fn prop_u32s(&mut self, n: &str, v: &[u32]) {
        let b: Vec<u8> = v.iter().flat_map(|x| x.to_be_bytes()).collect();
        self.prop(n, &b);
    }
    fn prop_u32(&mut self, n: &str, v: u32) {
        self.prop_u32s(n, &[v]);
    }
    fn prop_u64(&mut self, n: &str, v: u64) {
        self.prop_u32s(n, &[(v >> 32) as u32, v as u32]);
    }
    fn prop_empty(&mut self, n: &str) {
        self.prop(n, &[]);
    }
}

/// The device tree blob: `bootargs`, the initrd range, `ram_size`, and one virtio-mmio node per
/// attached device (`virtio`: (base, SPI) of each).
pub fn build(
    bootargs: &str,
    initrd_start: u64,
    initrd_end: u64,
    ram_size: u64,
    virtio: &[(u64, u32)],
) -> Vec<u8> {
    let mut w = W {
        st: Vec::new(),
        strs: Vec::new(),
    };
    w.node("");
    w.prop_u32("#address-cells", 2);
    w.prop_u32("#size-cells", 2);
    w.prop_str("compatible", "linux,dummy-virt");
    w.prop_str("model", "hydo-hvm");
    w.prop_u32("interrupt-parent", 1);
    w.node("chosen");
    w.prop_str("bootargs", bootargs);
    w.prop_str("stdout-path", "/uart@9000000");
    w.prop_u64("linux,initrd-start", initrd_start);
    w.prop_u64("linux,initrd-end", initrd_end);
    let seed: Vec<u8> = (0..64).map(|i| (0x5a + 7 * i) as u8).collect();
    w.prop("rng-seed", &seed);
    w.endnode();
    w.node("memory@40000000");
    w.prop_str("device_type", "memory");
    w.prop_u32s(
        "reg",
        &[0, RAM_BASE as u32, (ram_size >> 32) as u32, ram_size as u32],
    );
    w.endnode();
    w.node("cpus");
    w.prop_u32("#address-cells", 1);
    w.prop_u32("#size-cells", 0);
    w.node("cpu@0");
    w.prop_str("device_type", "cpu");
    w.prop_str("compatible", "arm,arm-v8");
    w.prop_u32("reg", 0);
    w.prop_str("enable-method", "psci");
    w.endnode();
    w.endnode();
    w.node("psci");
    w.prop_str("compatible", "arm,psci-0.2");
    w.prop_str("method", "hvc");
    w.endnode();
    w.node("intc@8000000");
    w.prop_str("compatible", "arm,cortex-a15-gic");
    w.prop_u32("#interrupt-cells", 3);
    w.prop_empty("interrupt-controller");
    w.prop_u32s(
        "reg",
        &[0, GICD_BASE as u32, 0, 0x1000, 0, GICC_BASE as u32, 0, 0x2000],
    );
    w.prop_u32("phandle", 1);
    w.endnode();
    w.node("timer");
    w.prop_str("compatible", "arm,armv8-timer");
    // only the secure and non-secure PHYSICAL timer PPIs: no virtual PPI, so Linux picks the physical timer
    w.prop_u32s("interrupts", &[1, 13, 0x104, 1, TIMER_PPI, 0x104]);
    w.prop_empty("always-on");
    w.endnode();
    w.node("uart@9000000");
    w.prop_str("compatible", "ns16550a");
    w.prop_u32s("reg", &[0, UART_BASE as u32, 0, 0x100]);
    w.prop_u32("clock-frequency", 1_843_200);
    w.prop_u32s("interrupts", &[0, UART_SPI, 4]);
    w.endnode();
    for &(b, spi) in virtio {
        w.node(&format!("virtio_mmio@{b:x}"));
        w.prop_str("compatible", "virtio,mmio");
        w.prop_u32s(
            "reg",
            &[0, b as u32, 0, crate::virtio::MMIO_SIZE as u32],
        );
        w.prop_u32s("interrupts", &[0, spi, 4]);
        w.prop_empty("dma-coherent");
        w.endnode();
    }
    w.endnode();
    w.s32(9);

    let (fs_n, fstr_n) = (w.st.len(), w.strs.len());
    let off_rsv = 40usize;
    let off_struct = off_rsv + 16;
    let off_str = off_struct + fs_n;
    let total = off_str + fstr_n;
    let mut out = vec![0u8; total];
    let be = |o: &mut [u8], at: usize, v: u32| o[at..at + 4].copy_from_slice(&v.to_be_bytes());
    be(&mut out, 0, 0xd00d_feed);
    be(&mut out, 4, total as u32);
    be(&mut out, 8, off_struct as u32);
    be(&mut out, 12, off_str as u32);
    be(&mut out, 16, off_rsv as u32);
    be(&mut out, 20, 17);
    be(&mut out, 24, 16);
    be(&mut out, 28, 0);
    be(&mut out, 32, fstr_n as u32);
    be(&mut out, 36, fs_n as u32);
    out[off_struct..off_struct + fs_n].copy_from_slice(&w.st);
    out[off_str..].copy_from_slice(&w.strs);
    out
}
