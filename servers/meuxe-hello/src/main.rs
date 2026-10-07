#![no_std]
#![no_main]

use core::arch::global_asm;
use meuxe_abi::USER_CHILD_SHARE;

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

const MSG: &[u8] = b"hello from disk";

#[no_mangle]
extern "C" fn main() -> ! {
    let dst = USER_CHILD_SHARE as *mut u8;
    for (index, byte) in MSG.iter().enumerate() {
        unsafe {
            dst.add(index).write_volatile(*byte);
        }
    }
    meuxe_rt::exit(0);
}
