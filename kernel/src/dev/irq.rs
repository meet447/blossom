//! Virtio completion interrupts.
//!
//! Vector 33 is virtio-blk, 34 is the tablet, and 35 is the keyboard.
//! The handler only reads that vector's ISR byte and sets a flag.
//! The syscall prints the line and returns to the driver.

use crate::arch::x86_64::cpu;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const BASE: usize = 33;
const COUNT: usize = 3;

struct Lane {
    fired: AtomicBool,
    isr: AtomicU64,
}

static LANES: [Lane; COUNT] = [
    Lane {
        fired: AtomicBool::new(false),
        isr: AtomicU64::new(0),
    },
    Lane {
        fired: AtomicBool::new(false),
        isr: AtomicU64::new(0),
    },
    Lane {
        fired: AtomicBool::new(false),
        isr: AtomicU64::new(0),
    },
];

pub fn arm(vector: u8, isr_virt: u64) {
    let Some(lane) = lane(vector) else {
        return;
    };
    lane.isr.store(isr_virt, Ordering::Release);
}

/// Interrupt context. No allocation, logging, or locks.
pub fn signal(vector: u8) {
    let Some(lane) = lane(vector) else {
        return;
    };
    let isr = lane.isr.load(Ordering::Relaxed);
    if isr != 0 {
        unsafe {
            (isr as *const u8).read_volatile();
        }
    }
    lane.fired.store(true, Ordering::Release);
}

/// `a0` and `a1` are vectors. Zero means virtio-blk, so the block driver
/// can keep passing zeros. A flag that is already set is consumed here and
/// is not cleared on the way in.
pub fn wait(a0: u64, a1: u64) {
    let first = normalize(a0);
    let second = normalize(a1);
    loop {
        if take(first) {
            announce(first);
            return;
        }
        if second != first && take(second) {
            announce(second);
            return;
        }
        cpu::sti();
        cpu::hlt();
        cpu::cli();
    }
}

fn normalize(vector: u64) -> u8 {
    if vector == 0 {
        BASE as u8
    } else {
        vector as u8
    }
}

fn take(vector: u8) -> bool {
    lane(vector)
        .map(|lane| lane.fired.swap(false, Ordering::AcqRel))
        .unwrap_or(false)
}

fn announce(vector: u8) {
    match vector {
        33 => crate::kprintln!("meuxe: blk irq"),
        34 => crate::kprintln!("meuxe: tablet irq"),
        35 => crate::kprintln!("meuxe: kbd irq"),
        _ => {}
    }
}

fn lane(vector: u8) -> Option<&'static Lane> {
    let index = vector as usize;
    if index < BASE || index >= BASE + COUNT {
        None
    } else {
        Some(&LANES[index - BASE])
    }
}
