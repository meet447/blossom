//! Virtio completion interrupts.
//!
//! Lanes cover vectors 32 through 47. Vector 32 is the local APIC timer.
//! Vector 33 is virtio-blk, 34 is the tablet, and 35 is the keyboard.
//! The handler only reads that vector's ISR byte and sets a flag.
//! The syscall prints the device line and returns to the driver.
//!
//! `SYS_WAIT_IRQ` sleeps only on vector 32 or on an `Irq` capability the
//! caller holds. A zero argument means that task's first `Irq` capability,
//! which is how the block driver keeps passing zeros.

use crate::arch::x86_64::cpu;
use crate::cap;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use meuxe_cap::TaskId;

const BASE: usize = 32;
const COUNT: usize = 16;

struct Lane {
    fired: AtomicBool,
    isr: AtomicU64,
}

static LANES: [Lane; COUNT] = [const {
    Lane {
        fired: AtomicBool::new(false),
        isr: AtomicU64::new(0),
    }
}; COUNT];

pub fn announce() {
    crate::kprintln!("meuxe: irq lanes={}-{}", BASE, BASE + COUNT - 1);
}

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

/// `a0` and `a1` are vectors. Zero means the caller's first `Irq` capability.
/// Vector 32 is the timer and is allowed for every task. A flag that is
/// already set is consumed here.
pub fn wait(task: u64, a0: u64, a1: u64) {
    let first = resolve(task, a0);
    let second = resolve(task, a1);
    let Some(first) = first else {
        if let Some(only) = second {
            sleep_until(only, only);
        }
        return;
    };
    let second = second.unwrap_or(first);
    sleep_until(first, second);
}

fn sleep_until(first: u8, second: u8) {
    loop {
        if take(first) {
            announce_vector(first);
            return;
        }
        if second != first && take(second) {
            announce_vector(second);
            return;
        }
        cpu::sti();
        cpu::hlt();
        cpu::cli();
    }
}

fn resolve(task: u64, vector: u64) -> Option<u8> {
    if vector == 32 {
        return Some(32);
    }
    let id = TaskId::new(task as u8)?;
    cap::with_mut(|caps| {
        if vector == 0 {
            caps.first_irq(id).map(|value| value as u8)
        } else if caps.has_irq(id, vector as u32) {
            Some(vector as u8)
        } else {
            None
        }
    })
}

fn take(vector: u8) -> bool {
    lane(vector)
        .map(|lane| lane.fired.swap(false, Ordering::AcqRel))
        .unwrap_or(false)
}

fn announce_vector(vector: u8) {
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
