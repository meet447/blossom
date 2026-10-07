//! Per-CPU round-robin.
//!
//! The timer interrupt may switch tasks by returning a different register
//! frame. It never allocates and never waits on a queue lock. The bootstrap
//! processor's current context is task 0 (the boot thread). Application
//! processors fall back to an idle task that is never queued.

use crate::arch::hlt;
use crate::arch::x86_64::cpu;
use crate::arch::x86_64::gdt;
use crate::arch::x86_64::idt::Frame;
use crate::arch::x86_64::percpu::{self, PerCpu};
use crate::mm::layout::{USER_STACK, USER_TEXT};
use crate::task::{self, IDLE, USER_STUB};
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

const MAX_TASKS: usize = task::MAX;
const KIND_FREE: u8 = 0;
const KIND_IDLE: u8 = 1;
const KIND_KERNEL: u8 = 2;
const KIND_USER: u8 = 3;
const KIND_ZOMBIE: u8 = 4;
const QUANTUM: u64 = 3;

static TICKS: AtomicU64 = AtomicU64::new(0);
static READY: AtomicU8 = AtomicU8::new(0);
pub static AP_COUNT: AtomicU64 = AtomicU64::new(0);

#[repr(C, align(16))]
struct KStack([u8; 16384]);

static mut STACKS: [KStack; MAX_TASKS] = [const { KStack([0; 16384]) }; MAX_TASKS];

struct Task {
    kind: u8,
    affinity: u64,
    frame: *mut Frame,
    kstack_top: u64,
    /// Zero means the kernel page tables. A non-zero value is that task's PML4.
    cr3: u64,
    ran_on: AtomicU8,
}

unsafe impl Sync for Task {}

static mut TASKS: [Task; MAX_TASKS] = [const {
    Task {
        kind: KIND_FREE,
        affinity: 0,
        frame: core::ptr::null_mut(),
        kstack_top: 0,
        cr3: 0,
        ran_on: AtomicU8::new(0),
    }
}; MAX_TASKS];

pub fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn idle_until(extra_ticks: u64) {
    let start = ticks();
    while ticks().wrapping_sub(start) < extra_ticks {
        hlt();
    }
}

pub fn init(lapic_id: u32) {
    let top = stack_top(IDLE);
    unsafe {
        let task = &mut TASKS[IDLE as usize];
        task.kind = KIND_KERNEL;
        task.affinity = 1;
        task.kstack_top = top;
    }
    percpu::prepare(0, lapic_id, top, IDLE as u64);
    gdt::set_rsp0(0, top);
}

pub fn prepare_ap_idle(cpu_index: usize, lapic_id: u32) -> *mut PerCpu {
    debug_assert!(cpu_index < 8);
    let id = cpu_index as u8;
    let top = stack_top(id);
    unsafe {
        let task = &mut TASKS[id as usize];
        task.kind = KIND_IDLE;
        task.affinity = 1u64 << cpu_index;
        task.kstack_top = top;
    }
    percpu::prepare(cpu_index, lapic_id, top, id as u64);
    percpu::ptr(cpu_index)
}

pub fn enable() {
    READY.store(1, Ordering::Release);
}

pub fn spawn_kernel(id: u8, entry: extern "C" fn(u64) -> !, arg: u64, affinity: u64) {
    forge(id, KIND_KERNEL, affinity, entry as u64, stack_top(id) - 8, 0x08, 0x10, arg);
    enqueue_on(0, id);
}

pub fn spawn_user() {
    let top = stack_top(USER_STUB);
    forge(
        USER_STUB,
        KIND_USER,
        1,
        USER_TEXT,
        USER_STACK + 0x1000 - 8,
        0x23,
        0x1b,
        0,
    );
    unsafe {
        TASKS[USER_STUB as usize].kstack_top = top;
    }
    enqueue_on(0, USER_STUB);
}

pub fn user_cr3(id: u8) -> u64 {
    task_cr3(id)
}

pub fn spawn_user_elf(id: u8, entry: u64, cr3: u64) {
    forge(
        id,
        KIND_USER,
        1,
        entry,
        crate::mm::layout::USER_STACK + 0x1000 - 8,
        0x23,
        0x1b,
        0,
    );
    unsafe {
        TASKS[id as usize].cr3 = cr3;
    }
    enqueue_on(0, id);
}

fn task_cr3(id: u8) -> u64 {
    let cr3 = unsafe { TASKS[id as usize].cr3 };
    if cr3 == 0 {
        crate::mm::kernel_cr3()
    } else {
        cr3
    }
}

pub fn ran_on(id: u8) -> u8 {
    unsafe { TASKS[id as usize].ran_on.load(Ordering::Acquire) }
}

pub fn steals(cpu: usize) -> u64 {
    unsafe { (*percpu::ptr(cpu)).steals.load(Ordering::Acquire) }
}

/// Timer entry. Returns the frame the interrupt stub should resume.
pub fn preempt(frame: *mut Frame) -> *mut Frame {
    let cpu = percpu::this();
    unsafe {
        (*cpu).ticks = (*cpu).ticks.wrapping_add(1);
        if (*cpu).cpu_index == 0 {
            TICKS.fetch_add(1, Ordering::Relaxed);
            let page = crate::service::net::tick_page();
            if page != 0 {
                let ticks = TICKS.load(Ordering::Relaxed);
                unsafe {
                    ((crate::mm::hhdm() + page) as *mut u64).write_volatile(ticks);
                }
            }
        }
        if READY.load(Ordering::Acquire) == 0 {
            return frame;
        }
        let force = (*cpu).yield_requested.load(Ordering::Acquire) != 0;
        if (*cpu).quantum_left > 1 && !force {
            (*cpu).quantum_left -= 1;
            return frame;
        }
    }
    let index = unsafe { (*cpu).cpu_index as usize };
    if !percpu::try_lock_queue(index) {
        return frame;
    }
    let mut stolen = false;
    let mut next = take(index, index);
    if next.is_none() {
        let victim = if index == 0 { 1 } else { 0 };
        if percpu::try_lock_queue(victim) {
            next = take(victim, index);
            if next.is_some() {
                stolen = true;
            }
            percpu::unlock_queue(victim);
        }
    }
    let Some(next) = next else {
        unsafe {
            (*cpu).quantum_left = QUANTUM;
            (*cpu).yield_requested.store(0, Ordering::Release);
        }
        percpu::unlock_queue(index);
        return frame;
    };
    unsafe {
        let current = (*cpu).current_task as u8;
        TASKS[current as usize].frame = frame;
        if TASKS[current as usize].kind != KIND_IDLE {
            push(index, current);
        }
        (*cpu).current_task = next as u64;
        (*cpu).kernel_rsp = TASKS[next as usize].kstack_top;
        let next_cr3 = task_cr3(next);
        if cpu::read_cr3() & 0x000F_FFFF_FFFF_F000 != next_cr3 {
            cpu::write_cr3(next_cr3);
        }
        (*cpu).quantum_left = QUANTUM;
        (*cpu).yield_requested.store(0, Ordering::Release);
        if stolen {
            (*cpu).steals.fetch_add(1, Ordering::Relaxed);
        }
        TASKS[next as usize].ran_on.store(index as u8, Ordering::Release);
        gdt::set_rsp0(index, TASKS[next as usize].kstack_top);
        let resume = TASKS[next as usize].frame;
        percpu::unlock_queue(index);
        resume
    }
}

pub extern "C" fn ap_proof(_arg: u64) -> ! {
    loop {
        AP_COUNT.fetch_add(1, Ordering::Release);
        cpu::hlt();
    }
}

fn forge(
    id: u8,
    kind: u8,
    affinity: u64,
    rip: u64,
    rsp: u64,
    cs: u64,
    ss: u64,
    arg: u64,
) {
    let top = stack_top(id);
    let frame = (top - core::mem::size_of::<Frame>() as u64) as *mut Frame;
    unsafe {
        core::ptr::write(
            frame,
            Frame {
                r15: 0,
                r14: 0,
                r13: 0,
                r12: 0,
                r11: 0,
                r10: 0,
                r9: 0,
                r8: 0,
                rdi: arg,
                rsi: 0,
                rbp: 0,
                rbx: 0,
                rdx: 0,
                rcx: 0,
                rax: 0,
                vector: 0,
                error: 0,
                rip,
                cs,
                rflags: 0x202,
                rsp,
                ss,
            },
        );
        let task = &mut TASKS[id as usize];
        task.kind = kind;
        task.affinity = affinity;
        task.frame = frame;
        task.kstack_top = top;
        task.cr3 = 0;
    }
}

fn stack_top(id: u8) -> u64 {
    unsafe { core::ptr::addr_of!(STACKS[id as usize]) as u64 + core::mem::size_of::<KStack>() as u64 }
}

fn enqueue_on(cpu: usize, id: u8) {
    let flags = lock_queue(cpu);
    push(cpu, id);
    unlock(cpu, flags);
}

fn lock_queue(cpu: usize) -> u64 {
    loop {
        let flags = cpu::read_flags();
        cpu::cli();
        if percpu::try_lock_queue(cpu) {
            return flags;
        }
        cpu::restore_flags(flags);
        core::hint::spin_loop();
    }
}

fn unlock(cpu: usize, flags: u64) {
    percpu::unlock_queue(cpu);
    cpu::restore_flags(flags);
}

fn push(cpu: usize, id: u8) {
    unsafe {
        let block = percpu::ptr(cpu);
        let len = (*block).queue_len as usize;
        if len < (*block).queue.len() {
            (*block).queue[len] = id;
            (*block).queue_len = (len + 1) as u64;
        }
    }
}

fn take(cpu: usize, for_cpu: usize) -> Option<u8> {
    unsafe {
        let block = percpu::ptr(cpu);
        let len = (*block).queue_len as usize;
        let mut found = None;
        let mut write = 0;
        for read in 0..len {
            let id = (*block).queue[read];
            if found.is_none() && allows(id, for_cpu) {
                found = Some(id);
            } else {
                (*block).queue[write] = id;
                write += 1;
            }
        }
        (*block).queue_len = write as u64;
        found
    }
}

fn allows(id: u8, cpu: usize) -> bool {
    let kind = unsafe { TASKS[id as usize].kind };
    if kind == KIND_ZOMBIE || kind == KIND_FREE {
        return false;
    }
    unsafe { TASKS[id as usize].affinity & (1u64 << cpu) != 0 }
}

pub fn retire(id: u8) {
    unsafe {
        TASKS[id as usize].kind = KIND_ZOMBIE;
    }
    purge_queues(id);
}

fn purge_queues(id: u8) {
    for cpu in 0..2 {
        if !percpu::try_lock_queue(cpu) {
            continue;
        }
        unsafe {
            let block = percpu::ptr(cpu);
            let len = (*block).queue_len as usize;
            let mut write = 0;
            for read in 0..len {
                let queued = (*block).queue[read];
                if queued != id {
                    (*block).queue[write] = queued;
                    write += 1;
                }
            }
            (*block).queue_len = write as u64;
        }
        percpu::unlock_queue(cpu);
    }
}

pub fn park_current() -> ! {
    let cpu = percpu::this();
    let current = unsafe { (*cpu).current_task as u8 };
    retire(current);
    unsafe {
        (*cpu).yield_requested.store(1, Ordering::Release);
    }
    loop {
        cpu::sti();
        cpu::hlt();
        cpu::cli();
    }
}
