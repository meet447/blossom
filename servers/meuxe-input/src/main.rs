//! Virtio-tablet and virtio-keyboard driver.
//!
//! Absolute axes go to the compositor. Key-down events go to the terminal.
//! Both queues use MSI-X. This task sleeps on vector 34 or 35 when nothing
//! is waiting to be sent. Both queues hold 32 buffers so one QMP batch of
//! key up/down events fits.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::sync::atomic::{fence, Ordering};
use meuxe_abi::{
    pack_key, pack_pointer_button, CompletionEntry, InputBoot, KeyboardBoot, SubmissionEntry,
    RESULT_OK,
    SYS_RING_PROCESS, SYS_WAIT_IRQ, USER_INFO, USER_KBD_INFO, SQ_OPCODE_SEND,
};
use meuxe_rt::{ring, syscall, yield_once};

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

const POINTER_CAP: u32 = 1;
const KEY_CAP: u32 = 2;
const QUEUE: u16 = 64;
const AVAIL: u64 = 0x400;
const USED: u64 = 0x500;
const ACKNOWLEDGE: u8 = 1;
const DRIVER: u8 = 2;
const DRIVER_OK: u8 = 4;
const FEATURES_OK: u8 = 8;
const VERSION_1: u32 = 1;
const DESC_WRITE: u16 = 2;
const EV_KEY: u16 = 1;
const BTN_LEFT: u16 = 0x110;
const BTN_TOUCH: u16 = 0x14a;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const POINTER_TAG: u64 = 4;
const KEY_TAG: u64 = 5;
const TABLET_VECTOR: u64 = 34;
const KEYBOARD_VECTOR: u64 = 35;

struct Dev {
    common: u64,
    notify: u64,
    notify_mul: u32,
    queue_phys: u64,
    queue_virt: u64,
    event_phys: u64,
    event_virt: u64,
    last_used: u16,
}

struct Keys {
    buf: [u16; 64],
    head: usize,
    len: usize,
}

#[no_mangle]
extern "C" fn main() -> ! {
    let tablet_boot = unsafe { &*(USER_INFO as *const InputBoot) };
    let keyboard_boot = unsafe { &*(USER_KBD_INFO as *const KeyboardBoot) };
    let mut tablet = Dev {
        common: tablet_boot.common,
        notify: tablet_boot.notify,
        notify_mul: tablet_boot.notify_mul,
        queue_phys: tablet_boot.queue_phys,
        queue_virt: tablet_boot.queue_virt,
        event_phys: tablet_boot.event_phys,
        event_virt: tablet_boot.event_virt,
        last_used: 0,
    };
    if !setup(&mut tablet) {
        unsafe {
            (tablet_boot.ready as *mut u8).write_volatile(2);
        }
        loop {
            yield_once();
        }
    }
    let mut keyboard = Dev {
        common: keyboard_boot.common,
        notify: keyboard_boot.notify,
        notify_mul: keyboard_boot.notify_mul,
        queue_phys: keyboard_boot.queue_phys,
        queue_virt: keyboard_boot.queue_virt,
        event_phys: keyboard_boot.event_phys,
        event_virt: keyboard_boot.event_virt,
        last_used: 0,
    };
    let keyboard_ok = setup(&mut keyboard);
    if !keyboard_ok {
        unsafe {
            (keyboard_boot.ready as *mut u8).write_volatile(2);
        }
    }
    unsafe {
        (tablet_boot.ready as *mut u8).write_volatile(1);
    }
    fence(Ordering::SeqCst);
    if keyboard_ok {
        unsafe {
            (keyboard_boot.ready as *mut u8).write_volatile(1);
        }
        fence(Ordering::SeqCst);
    }

    let width = tablet_boot.width;
    let height = tablet_boot.height;
    let mut seen_x = false;
    let mut seen_y = false;
    let mut abs_x = 0u32;
    let mut abs_y = 0u32;
    let mut last_sent: Option<(u32, u32, bool)> = None;
    let mut button = false;
    let mut pointer_inflight = false;
    let mut key_inflight = false;
    let mut pending_key = 0u16;
    let mut keys = Keys {
        buf: [0; 64],
        head: 0,
        len: 0,
    };
    loop {
        poll_tablet(
            &mut tablet,
            &mut seen_x,
            &mut seen_y,
            &mut abs_x,
            &mut abs_y,
            &mut button,
        );
        if keyboard_ok {
            poll_keys(&mut keyboard, &mut keys);
        }
        let mut done = [None; 2];
        drain(&mut done);
        settle_pointer(&done, &mut pointer_inflight, &mut last_sent);
        settle_key(&done, &mut key_inflight, pending_key, &mut keys);

        let mut pushed = false;
        if !pointer_inflight && seen_x && seen_y && (abs_x != 0 || abs_y != 0) {
            let px = scale(abs_x, width);
            let py = scale(abs_y, height);
            if last_sent != Some((px, py, button)) {
                if push_pointer(px, py, button) {
                    pointer_inflight = true;
                    last_sent = Some((px, py, button));
                    pushed = true;
                }
            }
        }
        if keyboard_ok && !key_inflight {
            if let Some(code) = keys.pop_front() {
                if push_key(code) {
                    key_inflight = true;
                    pending_key = code;
                    pushed = true;
                } else {
                    keys.push_front(code);
                }
            }
        }
        if pushed {
            syscall(SYS_RING_PROCESS, 0, 0);
            let mut after = [None; 2];
            drain(&mut after);
            settle_pointer(&after, &mut pointer_inflight, &mut last_sent);
            settle_key(&after, &mut key_inflight, pending_key, &mut keys);
        }
        let pointer_pending = !pointer_inflight
            && seen_x
            && seen_y
            && (abs_x != 0 || abs_y != 0)
            && last_sent != Some((scale(abs_x, width), scale(abs_y, height), button));
        let busy = pointer_inflight
            || key_inflight
            || pointer_pending
            || keys.len > 0
            || !caught_up(&tablet)
            || (keyboard_ok && !caught_up(&keyboard));
        if busy {
            yield_once();
        } else {
            syscall(SYS_WAIT_IRQ, TABLET_VECTOR, KEYBOARD_VECTOR);
        }
    }
}

fn caught_up(dev: &Dev) -> bool {
    read16(dev.queue_virt + USED, 2) == dev.last_used
}

fn settle_pointer(
    done: &[Option<i32>; 2],
    inflight: &mut bool,
    last_sent: &mut Option<(u32, u32, bool)>,
) {
    if !*inflight {
        return;
    }
    match done[0] {
        Some(RESULT_OK) => *inflight = false,
        Some(_) => {
            *inflight = false;
            *last_sent = None;
        }
        None => {}
    }
}

fn settle_key(done: &[Option<i32>; 2], inflight: &mut bool, pending: u16, keys: &mut Keys) {
    if !*inflight {
        return;
    }
    match done[1] {
        Some(RESULT_OK) => *inflight = false,
        Some(_) => {
            *inflight = false;
            keys.push_front(pending);
        }
        None => {}
    }
}

fn poll_tablet(
    dev: &mut Dev,
    seen_x: &mut bool,
    seen_y: &mut bool,
    abs_x: &mut u32,
    abs_y: &mut u32,
    button: &mut bool,
) {
    poll_used(dev, |kind, code, value| {
        if kind == EV_ABS && code == ABS_X {
            *abs_x = value;
            *seen_x = true;
        } else if kind == EV_ABS && code == ABS_Y {
            *abs_y = value;
            *seen_y = true;
        } else if kind == EV_KEY && (code == BTN_LEFT || code == BTN_TOUCH) {
            *button = value != 0;
        }
    });
}

fn poll_keys(dev: &mut Dev, keys: &mut Keys) {
    poll_used(dev, |kind, code, value| {
        if kind == EV_KEY && value == 1 {
            keys.push_back(code);
        }
    });
}

fn poll_used(dev: &mut Dev, mut each: impl FnMut(u16, u16, u32)) {
    let idx = read16(dev.queue_virt + USED, 2);
    while dev.last_used != idx {
        let slot = (dev.last_used % QUEUE) as u64;
        let elem = dev.queue_virt + USED + 4 + slot * 8;
        let id = read32(elem, 0) as u16;
        if id < QUEUE {
            let event = dev.event_virt + (id as u64) * 8;
            let kind = read16(event, 0);
            let code = read16(event, 2);
            let value = read32(event, 4);
            each(kind, code, value);
            requeue(dev, id);
        }
        dev.last_used = dev.last_used.wrapping_add(1);
    }
}

fn scale(value: u32, span: u32) -> u32 {
    if span <= 1 {
        return 0;
    }
    ((value as u64) * (span as u64 - 1) / 32767) as u32
}

fn push_pointer(px: u32, py: u32, held: bool) -> bool {
    push(SubmissionEntry {
        opcode: SQ_OPCODE_SEND,
        flags: 0,
        cap: POINTER_CAP,
        a: pack_pointer_button(px, py, held),
        b: 0,
        user_data: POINTER_TAG,
    })
}

fn push_key(code: u16) -> bool {
    push(SubmissionEntry {
        opcode: SQ_OPCODE_SEND,
        flags: 0,
        cap: KEY_CAP,
        a: pack_key(code),
        b: 0,
        user_data: KEY_TAG,
    })
}

fn push(entry: SubmissionEntry) -> bool {
    ring().sq.push(entry).is_ok()
}

fn drain(into: &mut [Option<i32>; 2]) {
    while let Some(entry) = ring().cq.pop() {
        store_result(into, entry);
    }
}

fn store_result(into: &mut [Option<i32>; 2], entry: CompletionEntry) {
    if entry.user_data == POINTER_TAG {
        into[0] = Some(entry.result);
    } else if entry.user_data == KEY_TAG {
        into[1] = Some(entry.result);
    }
}

impl Keys {
    fn push_back(&mut self, code: u16) {
        if self.len == self.buf.len() {
            return;
        }
        let tail = (self.head + self.len) % self.buf.len();
        self.buf[tail] = code;
        self.len += 1;
    }

    fn push_front(&mut self, code: u16) {
        if self.len == self.buf.len() {
            return;
        }
        self.head = (self.head + self.buf.len() - 1) % self.buf.len();
        self.buf[self.head] = code;
        self.len += 1;
    }

    fn pop_front(&mut self) -> Option<u16> {
        if self.len == 0 {
            return None;
        }
        let code = self.buf[self.head];
        self.head = (self.head + 1) % self.buf.len();
        self.len -= 1;
        Some(code)
    }
}

fn setup(dev: &mut Dev) -> bool {
    let common = dev.common;
    write8(common, 0x14, 0);
    let mut reset = false;
    for _ in 0..100_000 {
        if read8(common, 0x14) == 0 {
            reset = true;
            break;
        }
    }
    if !reset {
        return false;
    }
    write8(common, 0x14, ACKNOWLEDGE);
    write8(common, 0x14, ACKNOWLEDGE | DRIVER);
    write32(common, 0x00, 1);
    let high = read32(common, 0x04);
    if high & VERSION_1 == 0 {
        return false;
    }
    write32(common, 0x08, 1);
    write32(common, 0x0c, VERSION_1);
    write32(common, 0x08, 0);
    write32(common, 0x0c, 0);
    write8(common, 0x14, ACKNOWLEDGE | DRIVER | FEATURES_OK);
    if read8(common, 0x14) & FEATURES_OK == 0 {
        return false;
    }
    write16(common, 0x16, 0);
    let max = read16(common, 0x18);
    if max < QUEUE {
        return false;
    }
    write16(common, 0x18, QUEUE);
    write64(common, 0x20, dev.queue_phys);
    write64(common, 0x28, dev.queue_phys + AVAIL);
    write64(common, 0x30, dev.queue_phys + USED);
    write16(common, 0x1a, 0);
    if read16(common, 0x1a) == 0xFFFF {
        return false;
    }
    write16(common, 0x1c, 1);
    for index in 0..QUEUE as u64 {
        write_desc(
            dev.queue_virt,
            index,
            dev.event_phys + index * 8,
            8,
            DESC_WRITE,
            0,
        );
        write16(dev.queue_virt + AVAIL, 4 + index * 2, index as u16);
    }
    fence(Ordering::SeqCst);
    write16(dev.queue_virt + AVAIL, 2, QUEUE);
    fence(Ordering::SeqCst);
    write8(common, 0x14, ACKNOWLEDGE | DRIVER | FEATURES_OK | DRIVER_OK);
    notify(dev);
    true
}

fn requeue(dev: &Dev, id: u16) {
    let avail = dev.queue_virt + AVAIL;
    let idx = read16(avail, 2);
    let slot = (idx % QUEUE) as u64;
    write16(avail, 4 + slot * 2, id);
    fence(Ordering::SeqCst);
    write16(avail, 2, idx.wrapping_add(1));
    fence(Ordering::SeqCst);
    notify(dev);
}

fn notify(dev: &Dev) {
    let notify_off = read16(dev.common, 0x1e) as u64;
    let notify = dev.notify + notify_off * dev.notify_mul as u64;
    unsafe {
        (notify as *mut u16).write_volatile(0);
    }
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
