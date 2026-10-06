//! Per-CPU data addressed through `GS_BASE`.
//!
//! In the kernel, `GS_BASE` points here and `KERNEL_GS_BASE` is the user
//! base (zero). `swapgs` exchanges them on the way to and from ring 3.

use super::cpu;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const MAX_CPUS: usize = 8;
pub const KERNEL_RSP: usize = 16;
pub const USER_RSP: usize = 24;

const GS_BASE: u32 = 0xC000_0101;
const KERNEL_GS_BASE: u32 = 0xC000_0102;

#[repr(C)]
pub struct PerCpu {
    pub self_ptr: u64,
    pub cpu_index: u64,
    pub kernel_rsp: u64,
    pub user_rsp: u64,
    pub current_task: u64,
    pub lapic_id: u64,
    pub ticks: u64,
    pub quantum_left: u64,
    pub steals: AtomicU64,
    pub yield_requested: AtomicU64,
    pub queue_lock: AtomicBool,
    pub queue_len: u64,
    pub queue: [u8; 64],
}

const _: () = assert!(core::mem::offset_of!(PerCpu, kernel_rsp) == KERNEL_RSP);
const _: () = assert!(core::mem::offset_of!(PerCpu, user_rsp) == USER_RSP);

impl PerCpu {
    const fn empty() -> Self {
        Self {
            self_ptr: 0,
            cpu_index: 0,
            kernel_rsp: 0,
            user_rsp: 0,
            current_task: 0,
            lapic_id: 0,
            ticks: 0,
            quantum_left: 3,
            steals: AtomicU64::new(0),
            yield_requested: AtomicU64::new(0),
            queue_lock: AtomicBool::new(false),
            queue_len: 0,
            queue: [0; 64],
        }
    }
}

static mut PERCPU: [PerCpu; MAX_CPUS] = [const { PerCpu::empty() }; MAX_CPUS];

pub fn ptr(index: usize) -> *mut PerCpu {
    unsafe { core::ptr::addr_of_mut!(PERCPU).cast::<PerCpu>().add(index) }
}

pub fn prepare(index: usize, lapic_id: u32, kernel_rsp: u64, current_task: u64) {
    let cpu = ptr(index);
    unsafe {
        (*cpu).self_ptr = cpu as u64;
        (*cpu).cpu_index = index as u64;
        (*cpu).kernel_rsp = kernel_rsp;
        (*cpu).current_task = current_task;
        (*cpu).lapic_id = lapic_id as u64;
        (*cpu).quantum_left = 3;
    }
}

/// Point this CPU's `GS_BASE` at its prepared block.
pub fn bind(cpu: *mut PerCpu) {
    unsafe {
        (*cpu).self_ptr = cpu as u64;
    }
    cpu::wrmsr(GS_BASE, cpu as u64);
    cpu::wrmsr(KERNEL_GS_BASE, 0);
}

pub fn this() -> *mut PerCpu {
    let value: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, gs:[0]",
            out(reg) value,
            options(nostack, preserves_flags)
        );
    }
    value as *mut PerCpu
}

pub fn try_lock_queue(index: usize) -> bool {
    unsafe {
        (*ptr(index))
            .queue_lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
}

pub fn unlock_queue(index: usize) {
    unsafe {
        (*ptr(index)).queue_lock.store(false, Ordering::Release);
    }
}
