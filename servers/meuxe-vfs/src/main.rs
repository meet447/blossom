//! Mount the MXDF volume and answer path requests from the shell and Files.
//!
//! Block reads and writes are one 4096-byte page at `USER_SHARE + 4096`.
//! The request header sits at `USER_SHARE + 3200`.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::mem::MaybeUninit;
use core::sync::atomic::{fence, Ordering};
use meuxe_abi::{
    SubmissionEntry, ERR_AGAIN, RESULT_OK, SYS_REPORT, SYS_RING_PROCESS, USER_FS, USER_FS_FILES,
    USER_SHARE, SQ_OPCODE_RECV, SQ_OPCODE_SEND,
};
use meuxe_fs::mxdf::{BlockDev, Volume, BLOCK, CREATE, MxError, TRUNC};
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
const OP_STAT: u32 = 4;
const OP_MKDIR: u32 = 5;
const OP_UNLINK: u32 = 6;
const OP_RENAME: u32 = 7;
const OP_DF: u32 = 8;
const BLK_REQUEST: u64 = 1;
const STORE_TAG: u64 = 5;
const REPLY_TAG: u64 = 4;
const REPLY_BOX: u64 = RING + 0x810;
const REQ: u64 = 3200;
const DATA: u64 = USER_SHARE + 4096;

struct Disk;

impl BlockDev for Disk {
    fn read_block(&mut self, block: u32, buf: &mut [u8; BLOCK]) -> Result<(), MxError> {
        if !xfer(1, block) {
            return Err(MxError::Io);
        }
        unsafe {
            let src = DATA as *const u8;
            for index in 0..BLOCK {
                buf[index] = src.add(index).read_volatile();
            }
        }
        Ok(())
    }

    fn write_block(&mut self, block: u32, buf: &mut [u8; BLOCK]) -> Result<(), MxError> {
        unsafe {
            let dst = DATA as *mut u8;
            for index in 0..BLOCK {
                dst.add(index).write_volatile(buf[index]);
            }
        }
        fence(Ordering::SeqCst);
        if xfer(2, block) {
            Ok(())
        } else {
            Err(MxError::Io)
        }
    }
}

static mut VOLUME: MaybeUninit<Volume<Disk>> = MaybeUninit::uninit();

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
            unsafe {
                (BLK_BOX as *mut u64).write_volatile(0);
            }
            break;
        }
        syscall(SYS_RING_PROCESS, 0, 0);
        let mailbox = unsafe { (BLK_BOX as *const u64).read_volatile() };
        if mailbox != 0 {
            unsafe {
                (BLK_BOX as *mut u64).write_volatile(0);
            }
            break;
        }
        yield_once();
    }
    if mount() {
        report_mount();
        report_note();
    }
    serve();
}

fn vol() -> &'static mut Volume<Disk> {
    unsafe { VOLUME.assume_init_mut() }
}

fn mount() -> bool {
    match Volume::mount(Disk) {
        Ok(volume) => {
            unsafe {
                VOLUME.write(volume);
            }
            true
        }
        Err(_) => false,
    }
}

fn report_mount() {
    for (index, byte) in b"MOUNT".iter().enumerate() {
        unsafe {
            ((USER_SHARE + 3000 + index as u64) as *mut u8).write_volatile(*byte);
        }
    }
    syscall(SYS_REPORT, USER_SHARE + 3000, 5);
}

fn report_note() {
    let mut body = [0u8; 32];
    let len = match vol().read_at(b"/home/note", 0, &mut body) {
        Ok(len) => len,
        Err(_) => return,
    };
    if len == 0 {
        return;
    }
    unsafe {
        let dst = (USER_SHARE + 16) as *mut u8;
        for index in 0..len {
            dst.add(index).write_volatile(body[index]);
        }
    }
    syscall(SYS_REPORT, USER_SHARE + 16, len as u64);
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
    let path_len = (read_u32(page, 4) as usize).min(64);
    let mut path = [0u8; 64];
    for index in 0..path_len {
        path[index] = read_u8(page, 128 + index);
    }
    let aux_len = (read_u32(page, 192) as usize).min(64);
    let mut aux = [0u8; 64];
    for index in 0..aux_len {
        aux[index] = read_u8(page, 196 + index);
    }
    let data_len = (read_u32(page, 80) as usize).min(32);
    let mut data = [0u8; 32];
    for index in 0..data_len {
        data[index] = read_u8(page, 84 + index);
    }
    let flags = read_u32(page, 116);
    let mut out = [0u8; 48];
    let out_len = dispatch(
        op,
        &path[..path_len],
        &aux[..aux_len],
        &data[..data_len],
        flags,
        &mut out,
    );
    for index in 0..out_len {
        write_u8(page, 32 + index, out[index]);
    }
    write_u32(page, 28, out_len as u32);
    fence(Ordering::SeqCst);
    write_u32(page, 24, 1);
    fence(Ordering::SeqCst);
}

fn dispatch(
    op: u32,
    path: &[u8],
    aux: &[u8],
    data: &[u8],
    flags: u32,
    out: &mut [u8],
) -> usize {
    let volume = vol();
    let result = match op {
        OP_LIST => volume.list(path, out).map(|len| len.min(out.len())),
        OP_READ => volume.read_at(path, 0, out),
        OP_WRITE => {
            let bits = if flags == 0 { CREATE | TRUNC } else { flags };
            match volume.write_at(path, 0, data, bits) {
                Ok(_) if data.is_empty() => Ok(copy_ok(out)),
                Ok(_) => {
                    let n = data.len().min(out.len());
                    out[..n].copy_from_slice(&data[..n]);
                    Ok(n)
                }
                Err(error) => Err(error),
            }
        }
        OP_STAT => match volume.stat(path) {
            Ok(stat) => {
                let word = if stat.kind == 2 { b"dir" as &[u8] } else { b"file" };
                let n = word.len().min(out.len());
                out[..n].copy_from_slice(&word[..n]);
                Ok(n)
            }
            Err(error) => Err(error),
        },
        OP_MKDIR => volume.mkdir(path).map(|_| copy_ok(out)),
        OP_UNLINK => volume.unlink(path).map(|_| copy_ok(out)),
        OP_RENAME => volume.rename(path, aux).map(|_| copy_ok(out)),
        OP_DF => match volume.stat_fs() {
            Ok(info) => Ok(write_free(out, info.free_blocks)),
            Err(error) => Err(error),
        },
        _ => Err(MxError::Invalid),
    };
    result.unwrap_or(0)
}

fn copy_ok(out: &mut [u8]) -> usize {
    let word = b"ok";
    let n = word.len().min(out.len());
    out[..n].copy_from_slice(&word[..n]);
    n
}

fn write_free(out: &mut [u8], free: u32) -> usize {
    let prefix = b"free=";
    let mut tmp = [0u8; 16];
    let mut n = free;
    let mut index = tmp.len();
    if n == 0 {
        index -= 1;
        tmp[index] = b'0';
    }
    while n > 0 && index > 0 {
        index -= 1;
        tmp[index] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let digits = &tmp[index..];
    let mut len = 0usize;
    for byte in prefix.iter().chain(digits.iter()) {
        if len >= out.len() {
            break;
        }
        out[len] = *byte;
        len += 1;
    }
    len
}

fn xfer(op: u32, block: u32) -> bool {
    unsafe {
        ((USER_SHARE + REQ) as *mut u32).write_volatile(op);
        ((USER_SHARE + REQ + 4) as *mut u32).write_volatile(8);
        ((USER_SHARE + REQ + 8) as *mut u64).write_volatile(block as u64 * 8);
        ((USER_SHARE + REQ + 16) as *mut u32).write_volatile(0);
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
                Some(ERR_AGAIN) => {
                    yield_once();
                    break;
                }
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
    for _ in 0..400_000 {
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
