//! The guest's physical layout and the machine's constants (hvm.c).

pub const RAM_BASE: u64 = 0x4000_0000;
pub const KERNEL_IPA: u64 = RAM_BASE + 0x20_0000;
pub const DTB_IPA: u64 = RAM_BASE + 0x0800_0000;
pub const INITRD_IPA: u64 = RAM_BASE + 0x0820_0000;
pub const GICD_BASE: u64 = 0x0800_0000;
pub const GICC_BASE: u64 = 0x0801_0000;
pub const UART_BASE: u64 = 0x0900_0000;
/// non-secure physical timer: INTID 30
pub const TIMER_PPI: u32 = 14;
pub const TIMER_INTID: usize = 16 + TIMER_PPI as usize;
/// INTID 33
pub const UART_SPI: u32 = 1;
pub const UART_INTID: usize = 32 + UART_SPI as usize;
pub const NIRQ: usize = 64;
/// what the guest reads from CNTFRQ_EL0 (host value, constant)
pub const CNTFRQ: u64 = 24_000_000;
/// the console capture
pub const CONSOLE_MAX: usize = 1 << 22;
/// tick jitter's tick period: CONFIG_HZ_250
pub const TJ_PERIOD: u64 = CNTFRQ / 250;
pub const TJ_MAX_EXPLICIT: usize = 32;
pub const PCT_MAX: usize = 64;
/// the UART receive queue
pub const RXQ: usize = 4096;
/// the stage-2 protection granule (16 KiB host pages)
pub const PG: u64 = 0x4000;
/// the base of the kernel's linear map of the image (the single-step exclusive-load check)
pub const KIMAGE_VA: u64 = 0xffff_ffc0_8000_0000;

pub const BOOTARGS: &str =
    "console=ttyS0 earlycon=uart8250,mmio,0x09000000 clocksource.arm_arch_timer.evtstrm=0 \
nokaslr rdinit=/init loglevel=7 random.trust_bootloader=on";

/// Stop reasons of the run loop (`STOP_*`).
pub const STOP_MARKER: i32 = 1;
pub const STOP_HALT: i32 = 2;
pub const STOP_IDLE: i32 = 3;
pub const STOP_LIMIT: i32 = 4;
pub const STOP_SPIN: i32 = 5;
