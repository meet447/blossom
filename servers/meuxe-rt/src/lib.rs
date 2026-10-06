//! Shared entry and syscall sequence for Meuxe ring-3 programs.

#![no_std]

use core::panic::PanicInfo;
use meuxe_abi::RingPage;

pub const RING: u64 = 0x800000;

pub fn syscall(number: u64, a0: u64, a1: u64) -> u64 {
    let value: u64;
    unsafe {
        // The entry stub preserves rbx and the callee-saved registers.
        // Everything else the SysV ABI calls caller-saved is dead on return.
        core::arch::asm!(
            "syscall",
            in("rax") number,
            in("rdi") a0,
            in("rsi") a1,
            lateout("rax") value,
            lateout("rcx") _,
            lateout("r11") _,
            clobber_abi("sysv64"),
        );
    }
    value
}

pub fn ring() -> &'static RingPage {
    unsafe { &*(RING as *const RingPage) }
}

pub fn yield_once() {
    let task = syscall(meuxe_abi::SYS_TASK_ID, 0, 0);
    syscall(meuxe_abi::SYS_YIELD, 0, task);
}

pub fn exit(code: u32) -> ! {
    syscall(meuxe_abi::SYS_EXIT, code as u64, 0);
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
