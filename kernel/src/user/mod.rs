//! One mapped ring-3 stub. This is not an ELF loader.

use crate::mm::layout::{USER_RING, USER_STACK, USER_TEXT};
use crate::mm::{self, leaf_flags};
use core::arch::global_asm;

global_asm!(include_str!("stub.S"));

extern "C" {
    static user_stub_start: u8;
    static user_stub_end: u8;
}

const USER: u64 = 1 << 2;
const WRITABLE: u64 = 1 << 1;
const NX: u64 = 1 << 63;

pub fn setup() -> Result<u64, &'static str> {
    let text = mm::alloc_frame_zeroed()?;
    let stack = mm::alloc_frame_zeroed()?;
    let ring = mm::alloc_frame_zeroed()?;
    unsafe {
        let src = core::ptr::addr_of!(user_stub_start);
        let len = core::ptr::addr_of!(user_stub_end) as usize - src as usize;
        if len == 0 || len > 4096 {
            return Err("user stub is empty or larger than a page");
        }
        core::ptr::copy_nonoverlapping(src, (mm::hhdm() + text) as *mut u8, len);
    }
    mm::map_user_4k(USER_TEXT, text, false)?;
    mm::map_user_4k(USER_STACK, stack, true)?;
    mm::map_user_4k(USER_RING, ring, true)?;
    check(USER_TEXT, false)?;
    check(USER_STACK, true)?;
    check(USER_RING, true)?;
    Ok(ring)
}

fn check(virt: u64, writable: bool) -> Result<(), &'static str> {
    let flags = leaf_flags(virt).ok_or("user page is not mapped")?;
    if flags & USER == 0 {
        return Err("user page is supervisor");
    }
    if writable {
        if flags & WRITABLE == 0 || flags & NX == 0 {
            return Err("user data page is not writable no-execute");
        }
    } else if flags & WRITABLE != 0 || flags & NX != 0 {
        return Err("user text page is not read-execute");
    }
    Ok(())
}
