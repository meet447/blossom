//! Calculator window. Handle 1 sends dirty rectangles. Clicks arrive in the
//! pick page as a sequence, then x and y inside the surface.

#![no_std]
#![no_main]

use core::arch::global_asm;
use meuxe_abi::{CalcBoot, USER_INFO};
use meuxe_rt::yield_once;
use meuxe_ui::theme;
use meuxe_ui::{calc_key_at, damage, pick_at, pick_seq, Calc, CalcKey, Canvas};

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

const RECT_CAP: u32 = 1;

const LABELS: [&[u8]; 16] = [
    b"7", b"8", b"9", b"/", b"4", b"5", b"6", b"*", b"1", b"2", b"3", b"-", b"0", b"C", b"=",
    b"+",
];

#[no_mangle]
extern "C" fn main() -> ! {
    let boot = unsafe { &*(USER_INFO as *const CalcBoot) };
    let mut calc = Calc::new();
    let mut seen = 0u32;
    paint(boot, &calc);
    publish(boot);
    loop {
        let seq = pick_seq(boot.pick);
        if seq != 0 && seq != seen {
            seen = seq;
            let (x, y) = pick_at(boot.pick);
            if let Some(key) = calc_key_at(x, y, boot.scale) {
                calc.press(key);
                paint(boot, &calc);
                publish(boot);
            }
        }
        yield_once();
    }
}

fn publish(boot: &CalcBoot) {
    let scale = boot.scale.max(1);
    damage(
        RECT_CAP,
        boot.origin_x,
        boot.origin_y,
        boot.cols * 8 * scale,
        boot.rows * 8 * scale,
    );
}

fn paint(boot: &CalcBoot, calc: &Calc) {
    let scale = boot.scale.max(1);
    let width = boot.cols * 8 * scale;
    let height = boot.rows * 8 * scale;
    let canvas = Canvas::new(boot.surface as *mut u32, boot.stride, width, height);
    canvas.fill(theme::PANEL);
    let cell = 8 * scale;
    canvas.rect(cell, cell, width - cell * 2, cell * 2, theme::DISPLAY);
    let mut text = [0u8; 20];
    let n = calc.text(&mut text);
    let col = boot.cols.saturating_sub(n as u32 + 1);
    canvas.cells(col, 1, &text[..n], scale, theme::INK, theme::DISPLAY);
    for index in 0..16u32 {
        let grid_col = index % 4;
        let grid_row = index / 4;
        let origin_col = 1 + grid_col * 5;
        let origin_row = 4 + grid_row * 2;
        let key = calc_key_at(origin_col * cell + 1, origin_row * cell + 1, scale);
        let fill = match key {
            Some(CalcKey::Eq) | Some(CalcKey::Op(_)) => theme::ACCENT,
            Some(CalcKey::Clear) => theme::EDGE,
            _ => theme::TITLE,
        };
        canvas.rect(
            origin_col * cell,
            origin_row * cell,
            cell * 4,
            cell * 2,
            fill,
        );
        canvas.cells(
            origin_col + 1,
            origin_row,
            LABELS[index as usize],
            scale,
            theme::INK,
            fill,
        );
    }
}
