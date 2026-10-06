//! Limine application-processor release.
//!
//! Each AP enters on the bootloader's page tables, loads the kernel CR3, then
//! uses a private stack, GDT, and local APIC timer.

use super::cpu;
use super::gdt;
use super::idt;
use super::percpu::{self, PerCpu};
use super::{apic, syscall};
use crate::boot::BootInfo;
use crate::sched;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

global_asm!(
    ".section .text",
    ".global ap_trampoline",
    "ap_trampoline:",
    "cli",
    "cld",
    "mov rax, cr0",
    "btr rax, 2",
    "btr rax, 3",
    "bts rax, 1",
    "mov cr0, rax",
    "mov rax, cr4",
    "bts rax, 9",
    "bts rax, 10",
    "mov cr4, rax",
    "mov rax, qword ptr [rip + {cr3}]",
    "mov cr3, rax",
    "mov rbx, qword ptr [rdi + 24]",
    "mov rsp, qword ptr [rbx + 16]",
    "and rsp, -16",
    "sub rsp, 8",
    "mov rdi, rbx",
    "jmp {main}",
    cr3 = sym AP_CR3,
    main = sym ap_main,
);

#[no_mangle]
static AP_CR3: AtomicU64 = AtomicU64::new(0);

static CPUS_ONLINE: AtomicU64 = AtomicU64::new(0);

extern "C" {
    fn ap_trampoline();
}

pub fn start(boot: &BootInfo) -> Result<usize, &'static str> {
    if boot.mp_count < 2 {
        return Err("limine mp response has no application processor");
    }
    AP_CR3.store(cpu::read_cr3(), Ordering::Release);
    let mut released = 0usize;
    for index in 0..boot.mp_count {
        let info = boot.mp_cpus[index] as *const crate::boot::MpInfo;
        if info.is_null() {
            continue;
        }
        let lapic = unsafe { core::ptr::addr_of!((*info).lapic_id).read_volatile() };
        if lapic == boot.bsp_lapic_id {
            continue;
        }
        let cpu_index = released + 1;
        if cpu_index >= percpu::MAX_CPUS {
            break;
        }
        let cpu = sched::prepare_ap_idle(cpu_index, lapic);
        unsafe {
            (*info)
                .extra_argument
                .store(cpu as u64, Ordering::Release);
            (*info)
                .goto_address
                .store(ap_trampoline as u64, Ordering::SeqCst);
        }
        released += 1;
    }
    if released == 0 {
        return Err("no application processor was released");
    }
    let hz = boot.tsc_hz.max(1_000_000);
    let start = cpu::rdtsc();
    while CPUS_ONLINE.load(Ordering::Acquire) < released as u64 {
        if cpu::rdtsc().wrapping_sub(start) > hz.saturating_mul(2) {
            return Err("ap did not come online");
        }
        core::hint::spin_loop();
    }
    Ok(released + 1)
}

#[no_mangle]
extern "C" fn ap_main(cpu: *mut PerCpu) -> ! {
    let index = unsafe { (*cpu).cpu_index as usize };
    let rsp0 = unsafe { (*cpu).kernel_rsp };
    gdt::init_cpu(index, rsp0);
    idt::load();
    percpu::bind(cpu);
    syscall::init_cpu();
    cpu::write_cr3(AP_CR3.load(Ordering::Acquire));
    apic::init_ap();
    CPUS_ONLINE.fetch_add(1, Ordering::Release);
    cpu::sti();
    loop {
        cpu::hlt();
    }
}
