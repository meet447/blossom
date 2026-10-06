//! Meuxe kernel entry.
//!
//! Limine enters `_start` in long mode with paging already on. `init` replaces
//! those page tables, brings up the scheduler, then starts the userspace
//! servers. Task ids live in `task`. Devices live in `dev`. ELF loading lives
//! in `exec`.

#![no_std]
#![no_main]

extern crate alloc;

mod acpi;
mod arch;
mod boot;
mod cap;
mod console;
mod dev;
mod exec;
mod font;
mod init;
mod ipc;
mod log;
mod mm;
mod panic;
mod sched;
mod service;
mod sync;
mod task;
mod user;
mod verify;

use core::arch::global_asm;

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
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
    "and rsp, -16",
    "sub rsp, 8",
    "jmp {main}",
    main = sym kernel_main,
);

#[no_mangle]
extern "C" fn kernel_main() -> ! {
    init::run()
}
