//! I/O APIC redirection table. Every line stays masked.
//! Virtio-blk, the tablet, and the keyboard complete through MSI-X, which does not travel through this table.

use core::sync::atomic::{AtomicU64, Ordering};

static BASE: AtomicU64 = AtomicU64::new(0);

pub fn init(virt: u64, bsp_id: u32) -> u32 {
    BASE.store(virt, Ordering::Release);
    let version = read(virt, 1);
    let max_index = ((version >> 16) & 0xFF) as u32;
    let count = max_index + 1;
    for index in 0..count {
        let low = (0x30 + index) | (1 << 16);
        let high = bsp_id << 24;
        write(virt, 0x10 + index * 2, low);
        write(virt, 0x10 + index * 2 + 1, high);
    }
    count
}

fn read(base: u64, index: u32) -> u32 {
    unsafe {
        (base as *mut u32).write_volatile(index);
        ((base + 0x10) as *const u32).read_volatile()
    }
}

fn write(base: u64, index: u32, value: u32) {
    unsafe {
        (base as *mut u32).write_volatile(index);
        ((base + 0x10) as *mut u32).write_volatile(value);
    }
}
