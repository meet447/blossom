#![no_std]
#![no_main]

use core::arch::global_asm;

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

#[no_mangle]
extern "C" fn main() -> ! {
    unsafe {
        core::ptr::read_volatile(0xdead_0000 as *const u8);
    }
    meuxe_rt::exit(1);
}
