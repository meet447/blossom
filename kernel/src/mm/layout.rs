//! Virtual windows that must not collide with the higher-half direct map.
//!
//! The kernel image itself is linked at `-2 GiB`. These windows sit below that.

pub const HEAP_VIRT: u64 = 0xFFFF_FE80_0000_0000;
pub const HEAP_SIZE: u64 = 16 * 1024 * 1024;
pub const LAPIC_VIRT: u64 = 0xFFFF_FF80_0000_0000;
pub const IOAPIC_VIRT: u64 = 0xFFFF_FF80_0020_0000;
pub const IOAPIC_STRIDE: u64 = 0x1000;
/// MSI-X windows. Each device owns 16 KiB: table, a spare page, then the ISR.
/// Virtio-blk is first, then the tablet, then the keyboard.
pub const MSI_VIRT: u64 = 0xFFFF_FF80_0030_0000;
pub const MSI_STRIDE: u64 = 0x4000;
pub const MSI_DEVICES: u64 = 3;

/// Ring 3 proof pages. They sit in PML4 slot 0, away from the higher-half map.
pub const USER_TEXT: u64 = 0x0000_0000_0040_0000;
pub const USER_STACK: u64 = 0x0000_0000_0060_0000;
pub const USER_RING: u64 = 0x0000_0000_0080_0000;

extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __data_end: u8;
    static __bss_start: u8;
    static __bss_end: u8;
    static __got_start: u8;
    static __got_end: u8;
    static __kernel_end: u8;
}

fn addr(symbol: &u8) -> u64 {
    symbol as *const u8 as u64
}

pub fn text() -> (u64, u64) {
    unsafe { (addr(&__text_start), addr(&__text_end)) }
}

pub fn rodata() -> (u64, u64) {
    unsafe { (addr(&__rodata_start), addr(&__rodata_end)) }
}

pub fn data() -> (u64, u64) {
    unsafe { (addr(&__data_start), addr(&__data_end)) }
}

pub fn bss() -> (u64, u64) {
    unsafe { (addr(&__bss_start), addr(&__bss_end)) }
}

pub fn got() -> (u64, u64) {
    unsafe { (addr(&__got_start), addr(&__got_end)) }
}

pub fn kernel_end() -> u64 {
    unsafe { addr(&__kernel_end) }
}
