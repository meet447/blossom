//! Virtio-blk driver. Sector 0 and 1 are read through MSI-X, then written
//! to sector 2 and read back. A later request writes an updated directory
//! back to sector 0. The driver sleeps on the completion interrupt.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::sync::atomic::{fence, Ordering};
use meuxe_abi::{
    BlkBoot, SubmissionEntry, SYS_REPORT, SYS_RING_PROCESS, SYS_WAIT_IRQ, USER_INFO, USER_SHARE,
    SQ_OPCODE_RECV, SQ_OPCODE_SEND,
};
use meuxe_rt::{ring, syscall, yield_once, RING};

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

const ENDPOINT: u32 = 1;
const ACKNOWLEDGE: u8 = 1;
const DRIVER: u8 = 2;
const DRIVER_OK: u8 = 4;
const FEATURES_OK: u8 = 8;
const VERSION_1: u32 = 1;
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const DATA_LEN: u32 = 1024;

#[no_mangle]
extern "C" fn main() -> ! {
    let boot = unsafe { &*(USER_INFO as *const BlkBoot) };
    if let Some(notify) = prepare(boot) {
        if submit(boot, notify, 0, T_IN, true, 1, 0) && finish(boot, 1, 1040) {
            let word = unsafe { ((USER_SHARE + 16) as *const u64).read_volatile() };
            let send = SubmissionEntry {
                opcode: SQ_OPCODE_SEND,
                flags: 0,
                cap: ENDPOINT,
                a: word,
                b: 0,
                user_data: 2,
            };
            let _ = ring().sq.push(send);
            syscall(SYS_RING_PROCESS, 0, 0);
            syscall(SYS_REPORT, USER_SHARE + 16, 4);
            if copy_for_write(boot)
                && submit(boot, notify, 2048, T_OUT, false, 2, 2)
                && finish(boot, 2, 3088)
                && submit(boot, notify, 2048, T_IN, true, 3, 2)
                && finish(boot, 3, 3088)
            {
                syscall(SYS_REPORT, USER_SHARE + 2048 + 16, 4);
            }
            serve(boot, notify);
        }
    }
    loop {
        yield_once();
    }
}

fn serve(boot: &BlkBoot, notify: u64) -> ! {
    let mut posted = false;
    let mut next_idx = 4u16;
    loop {
        while ring().cq.pop().is_some() {}
        if !posted {
            let recv = SubmissionEntry {
                opcode: SQ_OPCODE_RECV,
                flags: 0,
                cap: ENDPOINT,
                a: RING + 0x800,
                b: 0,
                user_data: 4,
            };
            posted = ring().sq.push(recv).is_ok();
            if posted {
                syscall(SYS_RING_PROCESS, 0, 0);
            }
        }
        let mailbox = unsafe { ((RING + 0x800) as *const u64).read_volatile() };
        if mailbox != 0 {
            unsafe {
                ((RING + 0x800) as *mut u64).write_volatile(0);
            }
            let stored = write_directory(boot, notify, next_idx);
            next_idx = next_idx.wrapping_add(1);
            reply(if stored { 1 } else { 2 });
            if stored {
                syscall(SYS_REPORT, USER_SHARE + 2048 + 16, 4);
            }
            posted = false;
        }
        yield_once();
    }
}

fn write_directory(boot: &BlkBoot, notify: u64, idx: u16) -> bool {
    submit(boot, notify, 2048, T_OUT, false, idx, 0) && finish(boot, idx, 3088)
}

fn reply(code: u64) {
    for _ in 0..8 {
        let send = SubmissionEntry {
            opcode: SQ_OPCODE_SEND,
            flags: 0,
            cap: ENDPOINT,
            a: code,
            b: 0,
            user_data: 5,
        };
        if ring().sq.push(send).is_err() {
            yield_once();
            continue;
        }
        syscall(SYS_RING_PROCESS, 0, 0);
        loop {
            match ring().cq.pop() {
                Some(entry) if entry.user_data == 5 && entry.result == 0 => return,
                Some(entry) if entry.user_data == 5 => break,
                Some(_) => {}
                None => yield_once(),
            }
        }
    }
}

fn prepare(boot: &BlkBoot) -> Option<u64> {
    let common = boot.common;
    write8(common, 0x14, 0);
    let mut reset = false;
    for _ in 0..100_000 {
        if read8(common, 0x14) == 0 {
            reset = true;
            break;
        }
    }
    if !reset {
        return None;
    }
    write8(common, 0x14, ACKNOWLEDGE);
    write8(common, 0x14, ACKNOWLEDGE | DRIVER);
    write32(common, 0x00, 1);
    let high = read32(common, 0x04);
    if high & VERSION_1 == 0 {
        return None;
    }
    write32(common, 0x08, 1);
    write32(common, 0x0c, VERSION_1);
    write32(common, 0x08, 0);
    write32(common, 0x0c, 0);
    write8(common, 0x14, ACKNOWLEDGE | DRIVER | FEATURES_OK);
    if read8(common, 0x14) & FEATURES_OK == 0 {
        return None;
    }
    write16(common, 0x16, 0);
    let max = read16(common, 0x18);
    if max < 4 {
        return None;
    }
    write16(common, 0x18, 4);
    write64(common, 0x20, boot.queue_phys);
    write64(common, 0x28, boot.queue_phys + 0x80);
    write64(common, 0x30, boot.queue_phys + 0x800);
    write16(common, 0x1a, 0);
    if read16(common, 0x1a) == 0xFFFF {
        return None;
    }
    write16(common, 0x1c, 1);
    let notify_off = read16(common, 0x1e) as u64;
    write8(common, 0x14, ACKNOWLEDGE | DRIVER | FEATURES_OK | DRIVER_OK);
    Some(boot.notify + notify_off * boot.notify_mul as u64)
}

fn submit(
    boot: &BlkBoot,
    notify: u64,
    offset: u64,
    kind: u32,
    device_writes: bool,
    idx: u16,
    sector: u64,
) -> bool {
    let header = boot.share_virt + offset;
    let status_off = offset + 16 + DATA_LEN as u64;
    write32(header, 0, kind);
    write32(header, 4, 0);
    write64(header, 8, sector);
    write8(boot.share_virt, status_off, 0xFF);
    let data_flags = if device_writes {
        DESC_NEXT | DESC_WRITE
    } else {
        DESC_NEXT
    };
    write_desc(boot.queue_virt, 0, boot.share_phys + offset, 16, DESC_NEXT, 1);
    write_desc(
        boot.queue_virt,
        1,
        boot.share_phys + offset + 16,
        DATA_LEN,
        data_flags,
        2,
    );
    write_desc(
        boot.queue_virt,
        2,
        boot.share_phys + status_off,
        1,
        DESC_WRITE,
        0,
    );
    let slot = (idx - 1) & 3;
    write16(boot.queue_virt + 0x80, 4 + slot as u64 * 2, 0);
    fence(Ordering::SeqCst);
    write16(boot.queue_virt + 0x80, 2, idx);
    fence(Ordering::SeqCst);
    unsafe {
        (notify as *mut u16).write_volatile(0);
    }
    fence(Ordering::SeqCst);
    true
}

fn finish(boot: &BlkBoot, expect: u16, status_off: u64) -> bool {
    for _ in 0..4 {
        syscall(SYS_WAIT_IRQ, 0, 0);
        fence(Ordering::SeqCst);
        let idx = read16(boot.queue_virt + 0x800, 2);
        if idx >= expect {
            let status = read8(boot.share_virt, status_off);
            return status == 0;
        }
    }
    false
}

fn copy_for_write(boot: &BlkBoot) -> bool {
    for index in 0..DATA_LEN as u64 {
        let byte = read8(boot.share_virt, 16 + index);
        write8(boot.share_virt, 2048 + 16 + index, byte);
    }
    fence(Ordering::SeqCst);
    true
}

fn write_desc(base: u64, index: u64, addr: u64, len: u32, flags: u16, next: u16) {
    let slot = base + index * 16;
    unsafe {
        (slot as *mut u64).write_volatile(addr);
        ((slot + 8) as *mut u32).write_volatile(len);
        ((slot + 12) as *mut u16).write_volatile(flags);
        ((slot + 14) as *mut u16).write_volatile(next);
    }
}

fn read8(base: u64, off: u64) -> u8 {
    unsafe { ((base + off) as *const u8).read_volatile() }
}

fn read16(base: u64, off: u64) -> u16 {
    unsafe { ((base + off) as *const u16).read_volatile() }
}

fn read32(base: u64, off: u64) -> u32 {
    unsafe { ((base + off) as *const u32).read_volatile() }
}

fn write8(base: u64, off: u64, value: u8) {
    unsafe { ((base + off) as *mut u8).write_volatile(value) }
}

fn write16(base: u64, off: u64, value: u16) {
    unsafe { ((base + off) as *mut u16).write_volatile(value) }
}

fn write32(base: u64, off: u64, value: u32) {
    unsafe { ((base + off) as *mut u32).write_volatile(value) }
}

fn write64(base: u64, off: u64, value: u64) {
    unsafe { ((base + off) as *mut u64).write_volatile(value) }
}
