//! Publish `note` from the shared sector, then answer `ls`, `cat`, and `write`.
//!
//! The note report still points into the share page. Later directory
//! requests are served from a copy. `write` appends to that copy and asks
//! the block driver to store it back at sector 0.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{fence, Ordering};
use meuxe_abi::{
    SubmissionEntry, ERR_AGAIN, RESULT_OK, SYS_REPORT, SYS_RING_PROCESS, USER_FS, USER_FS_FILES,
    USER_SHARE,
    SQ_OPCODE_RECV, SQ_OPCODE_SEND,
};
use meuxe_fs::{append_record, Log};
use meuxe_rt::{ring, syscall, yield_once, RING};

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

const BLK_CAP: u32 = 1;
const FS_CAP: u32 = 3;
const FILES_CAP: u32 = 4;
const BLK_BOX: u64 = RING + 0x800;
const FS_BOX: u64 = RING + 0x808;
const FILES_BOX: u64 = RING + 0x818;
const OP_LIST: u32 = 1;
const OP_READ: u32 = 2;
const OP_WRITE: u32 = 3;
const BLK_REQUEST: u64 = 1;
const STORE_TAG: u64 = 5;
const REPLY_TAG: u64 = 4;
const REPLY_BOX: u64 = RING + 0x810;

struct LogBuf {
    bytes: UnsafeCell<[u8; 1024]>,
}

unsafe impl Sync for LogBuf {}

static LOG_BYTES: LogBuf = LogBuf {
    bytes: UnsafeCell::new([0; 1024]),
};

#[no_mangle]
extern "C" fn main() -> ! {
    let recv = SubmissionEntry {
        opcode: SQ_OPCODE_RECV,
        flags: 0,
        cap: BLK_CAP,
        a: BLK_BOX,
        b: 0,
        user_data: 1,
    };
    let _ = ring().sq.push(recv);
    loop {
        let mailbox = unsafe { (BLK_BOX as *const u64).read_volatile() };
        if mailbox != 0 {
            break;
        }
        syscall(SYS_RING_PROCESS, 0, 0);
        let mailbox = unsafe { (BLK_BOX as *const u64).read_volatile() };
        if mailbox != 0 {
            break;
        }
        yield_once();
    }
    report_note();
    copy_log();
    serve();
}

fn report_note() {
    let sector = unsafe { core::slice::from_raw_parts((USER_SHARE + 16) as *const u8, 1024) };
    let Ok(log) = Log::parse(sector) else {
        return;
    };
    let Some(note) = log.lookup(b"note") else {
        return;
    };
    syscall(SYS_REPORT, note.as_ptr() as u64, note.len() as u64);
}

fn copy_log() {
    unsafe {
        let dst = (*LOG_BYTES.bytes.get()).as_mut_ptr();
        let src = (USER_SHARE + 16) as *const u8;
        for index in 0..1024 {
            *dst.add(index) = src.add(index).read_volatile();
        }
    }
}

fn serve() -> ! {
    let mut shell_posted = false;
    let mut files_posted = false;
    loop {
        let (shell_done, files_done) = take_clients();
        if shell_done {
            shell_posted = false;
            if unsafe { (FS_BOX as *const u64).read_volatile() } != 0 {
                unsafe {
                    (FS_BOX as *mut u64).write_volatile(0);
                }
                answer(USER_FS);
            }
        }
        if files_done {
            files_posted = false;
            if unsafe { (FILES_BOX as *const u64).read_volatile() } != 0 {
                unsafe {
                    (FILES_BOX as *mut u64).write_volatile(0);
                }
                answer(USER_FS_FILES);
            }
        }
        if !shell_posted {
            shell_posted = post_recv(FS_CAP, FS_BOX, 3);
        }
        if !files_posted {
            files_posted = post_recv(FILES_CAP, FILES_BOX, 6);
        }
        syscall(SYS_RING_PROCESS, 0, 0);
        yield_once();
    }
}

fn post_recv(cap: u32, mailbox: u64, tag: u64) -> bool {
    let recv = SubmissionEntry {
        opcode: SQ_OPCODE_RECV,
        flags: 0,
        cap,
        a: mailbox,
        b: 0,
        user_data: tag,
    };
    ring().sq.push(recv).is_ok()
}

fn take_clients() -> (bool, bool) {
    let mut shell = false;
    let mut files = false;
    while let Some(entry) = ring().cq.pop() {
        if entry.user_data == 3 {
            shell = true;
        }
        if entry.user_data == 6 {
            files = true;
        }
    }
    (shell, files)
}

fn answer(page: u64) {
    let op = read_u32(page, 0);
    let name_len = (read_u32(page, 4) as usize).min(16);
    let mut name = [0u8; 16];
    for index in 0..name_len {
        name[index] = read_u8(page, 8 + index);
    }
    let mut out = [0u8; 48];
    let mut out_len = 0usize;
    let bytes = unsafe { &mut *LOG_BYTES.bytes.get() };
    if op == OP_WRITE {
        let data_len = (read_u32(page, 80) as usize).min(32);
        let mut data = [0u8; 32];
        for index in 0..data_len {
            data[index] = read_u8(page, 84 + index);
        }
        if name_len > 0
            && data_len > 0
            && append_record(bytes, &name[..name_len], &data[..data_len]).is_ok()
            && store_log(bytes)
        {
            if let Ok(log) = Log::parse(bytes) {
                if let Some(body) = log.lookup(&name[..name_len]) {
                    out_len = body.len().min(out.len());
                    out[..out_len].copy_from_slice(&body[..out_len]);
                }
            }
        }
    } else if let Ok(log) = Log::parse(bytes) {
        if op == OP_LIST {
            out_len = log.write_names(&mut out);
        } else if op == OP_READ {
            if let Some(data) = log.lookup(&name[..name_len]) {
                out_len = data.len().min(out.len());
                out[..out_len].copy_from_slice(&data[..out_len]);
            }
        }
    }
    for index in 0..out_len {
        write_u8(page, 32 + index, out[index]);
    }
    write_u32(page, 28, out_len as u32);
    fence(Ordering::SeqCst);
    write_u32(page, 24, 1);
    fence(Ordering::SeqCst);
}

fn store_log(bytes: &[u8]) -> bool {
    unsafe {
        let dst = (USER_SHARE + 2048 + 16) as *mut u8;
        for index in 0..1024 {
            dst.add(index).write_volatile(bytes[index]);
        }
    }
    fence(Ordering::SeqCst);
    signal_blk() && arm_reply() && wait_reply()
}

fn signal_blk() -> bool {
    for _ in 0..8 {
        let send = SubmissionEntry {
            opcode: SQ_OPCODE_SEND,
            flags: 0,
            cap: BLK_CAP,
            a: BLK_REQUEST,
            b: 0,
            user_data: STORE_TAG,
        };
        if ring().sq.push(send).is_err() {
            yield_once();
            continue;
        }
        syscall(SYS_RING_PROCESS, 0, 0);
        loop {
            match take_tag(STORE_TAG) {
                Some(RESULT_OK) => return true,
                Some(ERR_AGAIN) => break,
                Some(_) => return false,
                None => yield_once(),
            }
        }
    }
    false
}

fn arm_reply() -> bool {
    unsafe {
        (REPLY_BOX as *mut u64).write_volatile(0);
    }
    fence(Ordering::SeqCst);
    for _ in 0..8 {
        let recv = SubmissionEntry {
            opcode: SQ_OPCODE_RECV,
            flags: 0,
            cap: BLK_CAP,
            a: REPLY_BOX,
            b: 0,
            user_data: REPLY_TAG,
        };
        if ring().sq.push(recv).is_err() {
            yield_once();
            continue;
        }
        syscall(SYS_RING_PROCESS, 0, 0);
        match take_tag(REPLY_TAG) {
            Some(ERR_AGAIN) => continue,
            Some(RESULT_OK) => return true,
            Some(_) => return false,
            None => return true,
        }
    }
    false
}

fn wait_reply() -> bool {
    for _ in 0..200_000 {
        syscall(SYS_RING_PROCESS, 0, 0);
        let mailbox = unsafe { (REPLY_BOX as *const u64).read_volatile() };
        if mailbox != 0 {
            let _ = take_tag(REPLY_TAG);
            return mailbox == BLK_REQUEST;
        }
        yield_once();
    }
    false
}

fn take_tag(tag: u64) -> Option<i32> {
    let mut found = None;
    while let Some(entry) = ring().cq.pop() {
        if entry.user_data == tag {
            found = Some(entry.result);
        }
    }
    found
}

fn read_u8(page: u64, off: usize) -> u8 {
    unsafe { ((page + off as u64) as *const u8).read_volatile() }
}

fn read_u32(page: u64, off: usize) -> u32 {
    unsafe { ((page + off as u64) as *const u32).read_volatile() }
}

fn write_u8(page: u64, off: usize, value: u8) {
    unsafe { ((page + off as u64) as *mut u8).write_volatile(value) }
}

fn write_u32(page: u64, off: usize, value: u32) {
    unsafe { ((page + off as u64) as *mut u32).write_volatile(value) }
}
