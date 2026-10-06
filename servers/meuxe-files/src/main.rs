//! File manager window. Lists named records and shows the one under the pointer.
//!
//! Handle 1 sends the dirty rectangle to the compositor. Handle 2 is the
//! directory endpoint. Clicks arrive as a sequence number in the pick page.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::sync::atomic::{fence, Ordering};
use meuxe_abi::{FilesBoot, TERM_ACCENT, TERM_BG, TERM_FG, USER_FS, USER_INFO};
use meuxe_rt::yield_once;
use meuxe_ui::{damage, pick_at, pick_seq, Canvas, Dir};

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

const RECT_CAP: u32 = 1;
const FS_CAP: u32 = 2;
const MAX_NAMES: usize = 6;

#[no_mangle]
extern "C" fn main() -> ! {
    let boot = unsafe { &*(USER_INFO as *const FilesBoot) };
    let mut names = [[0u8; 16]; MAX_NAMES];
    let mut name_len = [0u8; MAX_NAMES];
    let mut count = 0usize;
    let mut body = [0u8; 48];
    let mut body_len = 0usize;
    let mut selected: Option<usize> = None;
    let mut seen = 0u32;
    let mut ticks = 0u32;
    refresh(
        boot,
        &mut names,
        &mut name_len,
        &mut count,
        &mut selected,
        &mut body,
        &mut body_len,
    );
    paint(boot, &names, &name_len, count, selected, &body[..body_len]);
    publish(boot);
    loop {
        ticks = ticks.wrapping_add(1);
        let seq = pick_seq(boot.pick);
        let clicked = seq != 0 && seq != seen;
        if clicked {
            seen = seq;
            let (_, py) = pick_at(boot.pick);
            let row = py / (8 * boot.scale.max(1));
            if row >= 1 {
                let index = (row - 1) as usize;
                if index < count {
                    selected = Some(index);
                }
            }
        }
        if clicked || (count == 0 && ticks % 32 == 0) || ticks % 400 == 0 {
            refresh(
                boot,
                &mut names,
                &mut name_len,
                &mut count,
                &mut selected,
                &mut body,
                &mut body_len,
            );
            paint(boot, &names, &name_len, count, selected, &body[..body_len]);
            publish(boot);
        }
        yield_once();
    }
}

fn refresh(
    boot: &FilesBoot,
    names: &mut [[u8; 16]; MAX_NAMES],
    name_len: &mut [u8; MAX_NAMES],
    count: &mut usize,
    selected: &mut Option<usize>,
    body: &mut [u8; 48],
    body_len: &mut usize,
) {
    let _ = boot;
    let mut fresh = [[0u8; 16]; MAX_NAMES];
    let mut fresh_len = [0u8; MAX_NAMES];
    let listed = list_names(&mut fresh, &mut fresh_len);
    if listed > 0 {
        *count = listed;
        *names = fresh;
        *name_len = fresh_len;
        if let Some(index) = *selected {
            if index >= *count {
                *selected = None;
                *body_len = 0;
            }
        }
    }
    if let Some(index) = *selected {
        if index < *count {
            *body_len = read_record(&names[index][..name_len[index] as usize], body);
        }
    }
}

fn publish(boot: &FilesBoot) {
    let scale = boot.scale.max(1);
    damage(
        RECT_CAP,
        boot.origin_x,
        boot.origin_y,
        boot.cols * 8 * scale,
        boot.rows * 8 * scale,
    );
}

fn directory() -> Dir {
    Dir {
        page: USER_FS,
        cap: FS_CAP,
    }
}

fn list_names(names: &mut [[u8; 16]; MAX_NAMES], lens: &mut [u8; MAX_NAMES]) -> usize {
    let mut out = [0u8; 48];
    let len = directory().list(&mut out);
    if len == 0 || out[0] == b'?' {
        return 0;
    }
    let mut count = 0usize;
    let mut index = 0usize;
    while index < len && count < MAX_NAMES {
        while index < len && out[index] == b' ' {
            index += 1;
        }
        let start = index;
        while index < len && out[index] != b' ' {
            index += 1;
        }
        if start == index {
            break;
        }
        let n = (index - start).min(16);
        names[count][..n].copy_from_slice(&out[start..start + n]);
        lens[count] = n as u8;
        count += 1;
    }
    count
}

fn read_record(name: &[u8], out: &mut [u8; 48]) -> usize {
    let len = directory().read(name, out);
    if len == 1 && out[0] == b'?' {
        0
    } else {
        len
    }
}

fn paint(
    boot: &FilesBoot,
    names: &[[u8; 16]; MAX_NAMES],
    lens: &[u8; MAX_NAMES],
    count: usize,
    selected: Option<usize>,
    body: &[u8],
) {
    let cols = boot.cols;
    let rows = boot.rows;
    let scale = boot.scale.max(1);
    let canvas = Canvas::new(boot.surface as *mut u32, boot.stride, cols * 8 * scale, rows * 8 * scale);
    canvas.fill(TERM_BG);
    canvas.text(0, 0, b"records", scale, TERM_ACCENT);
    for index in 0..count.min(rows as usize - 2) {
        let mark: &[u8] = if selected == Some(index) {
            b"> "
        } else {
            b"  "
        };
        let color = if selected == Some(index) {
            TERM_ACCENT
        } else {
            TERM_FG
        };
        let cell = 8 * scale;
        canvas.text(0, ((index as u32) + 1) * cell, mark, scale, color);
        canvas.text(
            2 * cell,
            ((index as u32) + 1) * cell,
            &names[index][..lens[index] as usize],
            scale,
            color,
        );
    }
    let body_row = (count as u32 + 2).min(rows.saturating_sub(1));
    let mut col = 0u32;
    let mut row = body_row;
    for byte in body {
        if col >= cols {
            col = 0;
            row += 1;
            if row >= rows {
                break;
            }
        }
        canvas.text(col * 8 * scale, row * 8 * scale, &[*byte], scale, TERM_FG);
        col += 1;
    }
    fence(Ordering::SeqCst);
}

