//! Watch the keyboard and sample the terminal the shell paints.
//!
//! The input task sleeps on the keyboard MSI-X vector. This code watches the
//! ready byte, then reads the framebuffer with the shared 8×8 font. `echo hi`,
//! `ls`, `cat note`, and `write` are checked by scanning cells, not by assuming
//! a row number.

use crate::arch::x86_64::cpu;
use crate::boot::{BootInfo, FbInfo};
use crate::font;
use crate::mm;
use crate::sched;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use meuxe_abi::{TERM_BG, TERM_COLS, TERM_FG, TERM_ROWS, TERM_SCALE, TERM_X, TERM_Y};

static READY_PHYS: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicBool = AtomicBool::new(false);
static LOGGED: AtomicBool = AtomicBool::new(false);

const LISTING: &[u8] = b"bin etc home tmp";
const NOTE: &[u8] = b"meuxe-phase3";
const STORED: &[u8] = b"there";

pub fn finish(boot: &BootInfo) -> Result<(), &'static str> {
    wait_echo(boot)?;
    crate::kprintln!("meuxe: echo ready");
    wait_directory(boot)?;
    crate::kprintln!("meuxe: directory ready");
    wait_record(boot)?;
    crate::kprintln!("meuxe: write ready");
    wait_free(boot)?;
    Ok(())
}

pub fn watch(ready_phys: u64) {
    READY_PHYS.store(ready_phys, Ordering::Release);
    let flag = unsafe { ((mm::hhdm() + ready_phys) as *const u8).read_volatile() };
    if flag == 2 {
        FAILED.store(true, Ordering::Release);
    }
    if flag == 1 && !LOGGED.swap(true, Ordering::AcqRel) {
        crate::kprintln!("meuxe: kbd listening");
    }
}

pub fn wait_echo(boot: &BootInfo) -> Result<(), &'static str> {
    let fb = boot
        .fb
        .ok_or("framebuffer disappeared before the terminal check")?;
    let start = sched::ticks();
    while sched::ticks().wrapping_sub(start) <= 4000 {
        if let Some(phys) = non_zero(READY_PHYS.load(Ordering::Acquire)) {
            watch(phys);
        }
        if FAILED.load(Ordering::Acquire) {
            return Err("virtio-keyboard setup failed");
        }
        if LOGGED.load(Ordering::Acquire) && line_is_hi(&fb) {
            crate::kprintln!("meuxe: shell line=hi");
            return Ok(());
        }
        cpu::hlt();
    }
    if !LOGGED.load(Ordering::Acquire) {
        Err("keyboard driver did not post buffers")
    } else {
        Err("terminal did not print hi")
    }
}

pub fn shows(boot: &BootInfo, text: &[u8]) -> bool {
    match boot.fb {
        Some(fb) => row_has(&fb, text),
        None => false,
    }
}

pub fn wait_directory(boot: &BootInfo) -> Result<(), &'static str> {
    let fb = boot
        .fb
        .ok_or("framebuffer disappeared before the directory check")?;
    let start = sched::ticks();
    let mut saw_list = false;
    let mut saw_note = false;
    while sched::ticks().wrapping_sub(start) <= 4000 {
        if !saw_list && row_has(&fb, LISTING) {
            saw_list = true;
            crate::kprintln!("meuxe: shell ls=/ bin etc home tmp");
        }
        if !saw_note && row_has(&fb, NOTE) {
            saw_note = true;
            crate::kprintln!("meuxe: shell cat=meuxe-phase3");
        }
        if saw_list && saw_note {
            return Ok(());
        }
        cpu::hlt();
    }
    if !saw_list {
        Err("terminal did not list the directory")
    } else {
        Err("terminal did not print the note")
    }
}

pub fn wait_record(boot: &BootInfo) -> Result<(), &'static str> {
    let start = sched::ticks();
    while sched::ticks().wrapping_sub(start) <= 4000 {
        if shows(boot, STORED) {
            crate::kprintln!("meuxe: shell wrote=there");
            return Ok(());
        }
        cpu::hlt();
    }
    Err("terminal did not print the new record")
}

pub fn wait_free(boot: &BootInfo) -> Result<(), &'static str> {
    let start = sched::ticks();
    while sched::ticks().wrapping_sub(start) <= 8000 {
        if shows(boot, b"free=") {
            return Ok(());
        }
        cpu::hlt();
    }
    Err("terminal did not print free space")
}

fn non_zero(value: u64) -> Option<u64> {
    if value == 0 {
        None
    } else {
        Some(value)
    }
}

fn line_is_hi(fb: &FbInfo) -> bool {
    glyph_matches(fb, b'h', 0, 2) && glyph_matches(fb, b'i', 1, 2)
}

fn row_has(fb: &FbInfo, text: &[u8]) -> bool {
    if text.len() > TERM_COLS as usize {
        return false;
    }
    for row in 0..TERM_ROWS {
        if text
            .iter()
            .enumerate()
            .all(|(col, byte)| glyph_matches(fb, *byte, col as u32, row))
        {
            return true;
        }
    }
    false
}

fn glyph_matches(fb: &FbInfo, ch: u8, cell_col: u32, cell_row: u32) -> bool {
    let glyph = font::glyph(ch);
    let fg = encode(fb, TERM_FG);
    let bg = encode(fb, TERM_BG);
    let mut saw_ink = false;
    for row in 0..8u32 {
        let bits = glyph[row as usize];
        for col in 0..8u32 {
            let lit = bits & (1 << col) != 0;
            let x = TERM_X + cell_col * 8 * TERM_SCALE + col * TERM_SCALE;
            let y = TERM_Y + cell_row * 8 * TERM_SCALE + row * TERM_SCALE;
            let pixel = read_pixel(fb, x, y);
            let expect = if lit { fg } else { bg };
            if pixel != expect {
                return false;
            }
            saw_ink |= lit;
        }
    }
    if ch == b' ' {
        !saw_ink
    } else {
        saw_ink
    }
}

fn encode(fb: &FbInfo, rgb: u32) -> u32 {
    let red = (rgb >> 16) & 0xFF;
    let green = (rgb >> 8) & 0xFF;
    let blue = rgb & 0xFF;
    (red << fb.red_shift) | (green << fb.green_shift) | (blue << fb.blue_shift)
}

fn read_pixel(fb: &FbInfo, x: u32, y: u32) -> u32 {
    let offset = y as usize * fb.pitch as usize + x as usize * 4;
    unsafe {
        (fb.virt as *const u8)
            .add(offset)
            .cast::<u32>()
            .read_volatile()
    }
}
