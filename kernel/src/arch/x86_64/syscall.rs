//! `syscall` / `sysret` entry. Ring 3 is entered the first time with `iretq`.

use super::cpu;
use super::percpu;
use crate::ipc;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

global_asm!(include_str!("syscall.S"));

const STAR: u32 = 0xC000_0081;
const LSTAR: u32 = 0xC000_0082;
const SFMASK: u32 = 0xC000_0084;
const EFER_SCE: u64 = 1;

static YIELD_STATUS: AtomicU64 = AtomicU64::new(u64::MAX);
static YIELD_TASK: AtomicU64 = AtomicU64::new(0);

extern "C" {
    fn syscall_entry();
}

pub fn init_cpu() {
    let efer = cpu::rdmsr(cpu::EFER);
    cpu::wrmsr(cpu::EFER, efer | EFER_SCE);
    cpu::wrmsr(STAR, (0x10 << 48) | (0x08 << 32));
    cpu::wrmsr(LSTAR, syscall_entry as u64);
    // TF, IF, DF are cleared for the duration of the syscall.
    cpu::wrmsr(SFMASK, 0x700);
}

pub fn yield_status() -> Option<(u64, u64)> {
    let status = YIELD_STATUS.load(Ordering::Acquire);
    if status == u64::MAX {
        None
    } else {
        Some((YIELD_TASK.load(Ordering::Acquire), status))
    }
}

#[no_mangle]
extern "C" fn syscall_dispatch(number: u64, a0: u64, a1: u64) -> u64 {
    let cpu = percpu::this();
    let task = unsafe { (*cpu).current_task };
    match number {
        meuxe_abi::SYS_TASK_ID => task,
        meuxe_abi::SYS_RING_PROCESS => {
            let completed = ipc::process_ring(task);
            crate::kprintln!("meuxe: syscall task={task} submit={completed}");
            completed as u64
        }
        meuxe_abi::SYS_REPORT => {
            crate::service::storage::note_report(task, a0, a1);
            0
        }
        meuxe_abi::SYS_WAIT_IRQ => {
            crate::dev::irq::wait(a0, a1);
            0
        }
        meuxe_abi::SYS_YIELD => {
            if YIELD_STATUS
                .compare_exchange(u64::MAX, a0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                YIELD_TASK.store(task, Ordering::Release);
                crate::kprintln!("meuxe: user yield status={a0} task={a1}");
            }
            unsafe {
                (*cpu).yield_requested.store(1, Ordering::Release);
            }
            cpu::sti();
            cpu::hlt();
            cpu::cli();
            0
        }
        _ => meuxe_abi::ERR_INVAL as u64,
    }
}
