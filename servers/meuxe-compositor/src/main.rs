//! Blit dirty rectangles onto the GOP framebuffer and hit-test a pointer.
//!
//! The first rectangle is the desktop window. Later rectangles are the
//! terminal or the file manager. A button-up sample hit-tests that window.
//! A button press drags a title bar, closes a frame, or opens one from its
//! desktop icon. Clients keep sending rectangles at the boot origins; this
//! task adds the distance the window has moved.

#![no_std]
#![no_main]

use core::arch::global_asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{fence, Ordering};
use meuxe_abi::{
    pack_pointer, pack_rect, CompositorBoot, SubmissionEntry, CALC_PX_H, CALC_PX_W, FILES_PX_H,
    FILES_PX_W, FRAME_BORDER, POINTER_HELD, SYS_RING_PROCESS, TERM_PX_H, TERM_PX_W, TERM_X, TERM_Y,
    TITLE_H, USER_INFO, WINDOW_H, WINDOW_W, WINDOW_X, WINDOW_Y, SQ_OPCODE_RECV,
};
use meuxe_font::paint_glyph;
use meuxe_rt::{ring, syscall, yield_once, RING};
use meuxe_ui::theme;

global_asm!(
    ".section .text.boot, \"ax\"",
    ".global _start",
    "_start:",
    "call main",
    "1:",
    "jmp 1b"
);

const CLIENT_CAP: u32 = 1;
const INPUT_CAP: u32 = 2;
const FILES_CAP: u32 = 4;
const CALC_CAP: u32 = 5;
const CLIENT_BOX: u64 = RING + 0x800;
const INPUT_BOX: u64 = RING + 0x808;
const FILES_BOX: u64 = RING + 0x810;
const CALC_BOX: u64 = RING + 0x818;
const FOCUS_TERM: u8 = 0;
const FOCUS_FILES: u8 = 1;
const FOCUS_CALC: u8 = 2;
/// Same fill the early console used, so the log can be erased without a flash.
const DESKTOP_BG: u32 = theme::DESKTOP;
const CURSOR_FILL: u32 = 0x00FF_FFFF;
const CURSOR_EDGE: u32 = 0x0010_141C;
const CURSOR_W: u32 = 12;
const CURSOR_H: u32 = 16;
const TITLE_BG: u32 = theme::TITLE;
const TITLE_FG: u32 = theme::INK;
const FRAME_EDGE: u32 = theme::EDGE;
const FRAME_ACCENT: u32 = theme::ACCENT;
const CLOSE_RGB: u32 = theme::CLOSE;
const SHADOW: u32 = theme::SHADOW;
const BAR_H: u32 = 28;
/// Left-hand desktop icons. They stay clear of the 16×16 window at (120, 120).
const ICON_X: u32 = 18;
const ICON_Y_TERM: u32 = 16;
const ICON_Y_FILES: u32 = 68;
const ICON_Y_CALC: u32 = 120;
const ICON_BOX: u32 = 32;
const ICON_HIT_W: u32 = 40;
const ICON_HIT_H: u32 = 44;

struct Desktop {
    term_x: u32,
    term_y: u32,
    files_x: u32,
    files_y: u32,
    calc_x: u32,
    calc_y: u32,
    term_open: bool,
    files_open: bool,
    calc_open: bool,
    focus: u8,
    win_x: u32,
    win_y: u32,
}

impl Desktop {
    fn term_rect(&self) -> Rect {
        self.rect(FOCUS_TERM)
    }

    fn files_rect(&self) -> Rect {
        self.rect(FOCUS_FILES)
    }

    fn rect(&self, kind: u8) -> Rect {
        let (x, y, w, h) = match kind {
            FOCUS_FILES => (self.files_x, self.files_y, FILES_PX_W, FILES_PX_H),
            FOCUS_CALC => (self.calc_x, self.calc_y, CALC_PX_W, CALC_PX_H),
            _ => (self.term_x, self.term_y, TERM_PX_W, TERM_PX_H),
        };
        content_rect(x, y, w, h)
    }

    fn is_open(&self, kind: u8) -> bool {
        match kind {
            FOCUS_FILES => self.files_open,
            FOCUS_CALC => self.calc_open,
            _ => self.term_open,
        }
    }

    fn set_open(&mut self, kind: u8, open: bool) {
        match kind {
            FOCUS_FILES => self.files_open = open,
            FOCUS_CALC => self.calc_open = open,
            _ => self.term_open = open,
        }
    }

    fn set_origin(&mut self, kind: u8, x: u32, y: u32) {
        match kind {
            FOCUS_FILES => {
                self.files_x = x;
                self.files_y = y;
            }
            FOCUS_CALC => {
                self.calc_x = x;
                self.calc_y = y;
            }
            _ => {
                self.term_x = x;
                self.term_y = y;
            }
        }
    }
}

/// Front window first.
fn order(desk: &Desktop) -> [u8; 3] {
    let mut kinds = [FOCUS_TERM, FOCUS_FILES, FOCUS_CALC];
    if let Some(pos) = kinds.iter().position(|kind| *kind == desk.focus) {
        kinds.swap(0, pos);
    }
    kinds
}

#[derive(Clone, Copy)]
struct Grab {
    kind: u8,
    dx: i32,
    dy: i32,
}

struct Stamp<const N: usize> {
    inner: UnsafeCell<StampInner<N>>,
}

struct StampInner<const N: usize> {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    live: bool,
    pixels: [u32; N],
}

unsafe impl<const N: usize> Sync for Stamp<N> {}

static CURSOR: Stamp<256> = Stamp {
    inner: UnsafeCell::new(StampInner {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        live: false,
        pixels: [0; 256],
    }),
};

static WINDOW_UNDER: Stamp<256> = Stamp {
    inner: UnsafeCell::new(StampInner {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        live: false,
        pixels: [0; 256],
    }),
};

#[no_mangle]
extern "C" fn main() -> ! {
    let boot = unsafe { &*(USER_INFO as *const CompositorBoot) };
    // The kernel console already drew the boot log into this framebuffer.
    // Erase it before accepting rectangles, or the terminal looks punched
    // through the log.
    clear_screen(boot);
    paint_mark(boot);
    let mut desk = Desktop {
        term_x: boot.term_x,
        term_y: boot.term_y,
        files_x: boot.files_x,
        files_y: boot.files_y,
        calc_x: boot.calc_x,
        calc_y: boot.calc_y,
        term_open: true,
        files_open: true,
        calc_open: true,
        focus: FOCUS_TERM,
        win_x: WINDOW_X,
        win_y: WINDOW_Y,
    };
    paint_frames(boot, &desk);
    paint_icons(boot);
    paint_taskbar(boot, &desk);
    post_recv(CLIENT_CAP, CLIENT_BOX, 1);
    post_recv(INPUT_CAP, INPUT_BOX, 2);
    post_recv(FILES_CAP, FILES_BOX, 4);
    post_recv(CALC_CAP, CALC_BOX, 5);
    let mut window_ready = false;
    let mut reported = false;
    let mut was_held = false;
    let mut cursor_at: Option<(u32, u32)> = None;
    let mut grab: Option<(i32, i32)> = None;
    let mut frame_drag: Option<Grab> = None;
    loop {
        while ring().cq.pop().is_some() {}
        let dirty = unsafe { (CLIENT_BOX as *const u64).read_volatile() };
        if dirty != 0 {
            unsafe {
                (CLIENT_BOX as *mut u64).write_volatile(0);
            }
            hide_cursor(boot);
            if desk.term_open || !is_term(boot, dirty) {
                if desk.files_open || !is_files(boot, dirty) {
                    if covers_window(dirty) {
                        let under = stamp::<256>(&WINDOW_UNDER);
                        if !under.live {
                            save_stamp(boot, under, desk.win_x, desk.win_y, WINDOW_W, WINDOW_H);
                        }
                        window_ready = true;
                    }
                    blit(boot, dirty, &desk);
                }
            }
            if (desk.win_x != WINDOW_X || desk.win_y != WINDOW_Y)
                && overlaps(dirty, desk.win_x, desk.win_y, WINDOW_W, WINDOW_H)
            {
                paint_window(boot, desk.win_x, desk.win_y);
            }
            if let Some((px, py)) = cursor_at {
                show_cursor(boot, px, py);
            }
            post_recv(CLIENT_CAP, CLIENT_BOX, 1);
        }
        let files_dirty = unsafe { (FILES_BOX as *const u64).read_volatile() };
        if files_dirty != 0 {
            unsafe {
                (FILES_BOX as *mut u64).write_volatile(0);
            }
            if desk.files_open {
                hide_cursor(boot);
                blit(boot, files_dirty, &desk);
                if let Some((px, py)) = cursor_at {
                    show_cursor(boot, px, py);
                }
            }
            post_recv(FILES_CAP, FILES_BOX, 4);
        }
        let calc_dirty = unsafe { (CALC_BOX as *const u64).read_volatile() };
        if calc_dirty != 0 {
            unsafe {
                (CALC_BOX as *mut u64).write_volatile(0);
            }
            if desk.calc_open {
                hide_cursor(boot);
                blit(boot, calc_dirty, &desk);
                if let Some((px, py)) = cursor_at {
                    show_cursor(boot, px, py);
                }
            }
            post_recv(CALC_CAP, CALC_BOX, 5);
        }
        let pointer = unsafe { (INPUT_BOX as *const u64).read_volatile() };
        if pointer != 0 && window_ready {
            unsafe {
                (INPUT_BOX as *mut u64).write_volatile(0);
            }
            let raw_x = pointer as u32;
            let held = raw_x & POINTER_HELD != 0;
            let px = raw_x & !POINTER_HELD;
            let py = (pointer >> 32) as u32;
            let pressed = held && !was_held;
            let released = !held && was_held;
            if !reported {
                let window = pack_rect(WINDOW_X, WINDOW_Y, WINDOW_W, WINDOW_H);
                let _ = hit(boot, window, pack_pointer(px, py));
                reported = true;
            } else if frame_drag.is_none()
                && !held
                && px >= desk.win_x
                && py >= desk.win_y
                && px < desk.win_x.saturating_add(WINDOW_W)
                && py < desk.win_y.saturating_add(WINDOW_H)
            {
                let (gdx, gdy) = grab.unwrap_or((
                    px as i32 - desk.win_x as i32,
                    py as i32 - desk.win_y as i32,
                ));
                grab = Some((gdx, gdy));
                let nx = clamp_origin(px as i32 - gdx, WINDOW_W, boot.width);
                let ny = clamp_origin(py as i32 - gdy, WINDOW_H, boot.height);
                if nx != desk.win_x || ny != desk.win_y {
                    relocate(boot, &mut desk.win_x, &mut desk.win_y, nx, ny);
                }
            } else if !held {
                grab = None;
            }
            if pressed {
                grab = None;
                frame_drag = handle_press(boot, &mut desk, px, py);
            }
            if held {
                if let Some(drag) = frame_drag {
                    drag_frame(boot, &mut desk, drag, px, py);
                }
            }
            if released {
                frame_drag = None;
            }
            was_held = held;
            show_cursor(boot, px, py);
            cursor_at = Some((px, py));
            post_recv(INPUT_CAP, INPUT_BOX, 2);
        }
        yield_once();
    }
}

fn handle_press(boot: &CompositorBoot, desk: &mut Desktop, px: u32, py: u32) -> Option<Grab> {
    for kind in order(desk) {
        if !desk.is_open(kind) {
            continue;
        }
        let rect = desk.rect(kind);
        if close_hit(rect, px, py) {
            close_frame(boot, desk, kind);
            return None;
        }
        if title_hit(rect, px, py) {
            if desk.focus != kind {
                raise(boot, desk, kind);
            }
            return Some(grab_at(desk.rect(kind), px, py, kind));
        }
        if in_outer(rect, px, py) {
            if point_in(rect, px, py) {
                if desk.focus != kind {
                    raise(boot, desk, kind);
                }
                deliver_pick(boot, desk, kind, px, py);
            }
            return None;
        }
    }
    if icon_hit(ICON_Y_TERM, px, py) {
        raise(boot, desk, FOCUS_TERM);
        return None;
    }
    if icon_hit(ICON_Y_FILES, px, py) {
        raise(boot, desk, FOCUS_FILES);
        return None;
    }
    if icon_hit(ICON_Y_CALC, px, py) {
        raise(boot, desk, FOCUS_CALC);
        return None;
    }
    if let Some(kind) = taskbar_hit(boot, desk, px, py) {
        raise(boot, desk, kind);
    }
    None
}

fn deliver_pick(boot: &CompositorBoot, desk: &Desktop, kind: u8, px: u32, py: u32) {
    if kind == FOCUS_FILES {
        note_pick(boot, desk.files_x, desk.files_y, px, py);
    } else if kind == FOCUS_CALC {
        note_calc(boot, desk.calc_x, desk.calc_y, px, py);
    }
}

fn grab_at(rect: Rect, px: u32, py: u32, kind: u8) -> Grab {
    Grab {
        kind,
        dx: px as i32 - rect.x as i32,
        dy: py as i32 - rect.y as i32,
    }
}

fn drag_frame(boot: &CompositorBoot, desk: &mut Desktop, drag: Grab, px: u32, py: u32) {
    let rect = desk.rect(drag.kind);
    let (nx, ny) = clamp_frame(
        px as i32 - drag.dx,
        py as i32 - drag.dy,
        rect.w,
        rect.h,
        boot.width,
        boot.height,
    );
    if nx != rect.x || ny != rect.y {
        move_frame(boot, desk, drag.kind, nx, ny);
    }
}

fn clamp_frame(x: i32, y: i32, w: u32, h: u32, limit_w: u32, limit_h: u32) -> (u32, u32) {
    let min_x = FRAME_BORDER as i32;
    let min_y = (TITLE_H + FRAME_BORDER) as i32;
    let max_x = limit_w.saturating_sub(w).saturating_sub(FRAME_BORDER) as i32;
    let max_y = limit_h.saturating_sub(h).saturating_sub(FRAME_BORDER) as i32;
    let x = clamp_span(x, min_x, max_x);
    let y = clamp_span(y, min_y, max_y);
    (x, y)
}

fn clamp_span(value: i32, min: i32, max: i32) -> u32 {
    let max = if max < min { min } else { max };
    if value < min {
        min as u32
    } else if value > max {
        max as u32
    } else {
        value as u32
    }
}

fn move_frame(boot: &CompositorBoot, desk: &mut Desktop, kind: u8, nx: u32, ny: u32) {
    let old = desk.rect(kind);
    let lifted = begin_edit(boot);
    fill_outer(boot, old);
    desk.set_origin(kind, nx, ny);
    end_edit(boot, desk, Some(outer(old)), lifted);
}

fn close_frame(boot: &CompositorBoot, desk: &mut Desktop, kind: u8) {
    let old = desk.rect(kind);
    let lifted = begin_edit(boot);
    fill_outer(boot, old);
    desk.set_open(kind, false);
    if desk.focus == kind {
        desk.focus = if desk.term_open {
            FOCUS_TERM
        } else if desk.files_open {
            FOCUS_FILES
        } else {
            FOCUS_CALC
        };
    }
    end_edit(boot, desk, Some(outer(old)), lifted);
}

fn raise(boot: &CompositorBoot, desk: &mut Desktop, kind: u8) {
    let lifted = begin_edit(boot);
    desk.set_open(kind, true);
    desk.focus = kind;
    end_edit(boot, desk, None, lifted);
}

fn begin_edit(boot: &CompositorBoot) -> bool {
    hide_cursor(boot);
    let under = stamp::<256>(&WINDOW_UNDER);
    if under.live {
        restore_stamp(boot, under);
        true
    } else {
        false
    }
}

fn end_edit(boot: &CompositorBoot, desk: &Desktop, hole: Option<Rect>, lifted: bool) {
    if let Some(hole) = hole {
        repair_mark(boot, hole, desk);
    }
    paint_icons(boot);
    paint_taskbar(boot, desk);
    blit_windows(boot, desk);
    paint_frames(boot, desk);
    if lifted {
        let under = stamp::<256>(&WINDOW_UNDER);
        save_stamp(boot, under, desk.win_x, desk.win_y, WINDOW_W, WINDOW_H);
        paint_window(boot, desk.win_x, desk.win_y);
    }
}

fn clamp_origin(value: i32, span: u32, limit: u32) -> u32 {
    let max = limit.saturating_sub(span) as i32;
    if value < 0 {
        0
    } else if value > max {
        max as u32
    } else {
        value as u32
    }
}

fn relocate(boot: &CompositorBoot, win_x: &mut u32, win_y: &mut u32, nx: u32, ny: u32) {
    hide_cursor(boot);
    let under = stamp::<256>(&WINDOW_UNDER);
    if under.live {
        restore_stamp(boot, under);
    } else {
        fill_rect(boot, *win_x, *win_y, WINDOW_W, WINDOW_H, DESKTOP_BG);
    }
    save_stamp(boot, under, nx, ny, WINDOW_W, WINDOW_H);
    paint_window(boot, nx, ny);
    *win_x = nx;
    *win_y = ny;
}

fn hide_cursor(boot: &CompositorBoot) {
    let cursor = stamp::<256>(&CURSOR);
    if cursor.live {
        restore_stamp(boot, cursor);
    }
}

fn show_cursor(boot: &CompositorBoot, px: u32, py: u32) {
    hide_cursor(boot);
    if px >= boot.width || py >= boot.height {
        return;
    }
    let w = CURSOR_W.min(boot.width - px);
    let h = CURSOR_H.min(boot.height - py);
    let cursor = stamp::<256>(&CURSOR);
    save_stamp(boot, cursor, px, py, w, h);
    for row in 0..h {
        for col in 0..w {
            if let Some(rgb) = cursor_pixel(col, row) {
                write_px(boot, px + col, py + row, encode(boot, rgb));
            }
        }
    }
    fence(Ordering::SeqCst);
}

fn cursor_pixel(col: u32, row: u32) -> Option<u32> {
    const SHAPE: [&[u8]; 16] = [
        b"#           ",
        b"##          ",
        b"#.#         ",
        b"#..#        ",
        b"#...#       ",
        b"#....#      ",
        b"#.....#     ",
        b"#......#    ",
        b"#.......#   ",
        b"#........#  ",
        b"#....###### ",
        b"#.#..#      ",
        b"## #..#     ",
        b"#  #..#     ",
        b"   #..#     ",
        b"   ####     ",
    ];
    let row = SHAPE.get(row as usize)?;
    match row.get(col as usize).copied() {
        Some(b'#') => Some(CURSOR_EDGE),
        Some(b'.') => Some(CURSOR_FILL),
        _ => None,
    }
}

fn paint_window(boot: &CompositorBoot, x: u32, y: u32) {
    let src = boot.back as *const u32;
    for row in 0..WINDOW_H {
        if y.saturating_add(row) >= boot.height {
            break;
        }
        for col in 0..WINDOW_W {
            if x.saturating_add(col) >= boot.width {
                break;
            }
            let pixel = encode(boot, unsafe {
                src.add((row * WINDOW_W + col) as usize).read_volatile()
            });
            write_px(boot, x + col, y + row, pixel);
        }
    }
    fence(Ordering::SeqCst);
}

fn fill_rect(boot: &CompositorBoot, x: u32, y: u32, w: u32, h: u32, rgb: u32) {
    let pixel = encode(boot, rgb);
    for row in 0..h {
        if y.saturating_add(row) >= boot.height {
            break;
        }
        for col in 0..w {
            if x.saturating_add(col) >= boot.width {
                break;
            }
            write_px(boot, x + col, y + row, pixel);
        }
    }
    fence(Ordering::SeqCst);
}

fn stamp<const N: usize>(slot: &Stamp<N>) -> &mut StampInner<N> {
    unsafe { &mut *slot.inner.get() }
}

fn save_stamp<const N: usize>(
    boot: &CompositorBoot,
    stamp: &mut StampInner<N>,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) {
    stamp.x = x;
    stamp.y = y;
    stamp.w = w;
    stamp.h = h;
    stamp.live = true;
    for row in 0..h {
        for col in 0..w {
            let index = (row * w + col) as usize;
            if index < N {
                stamp.pixels[index] = read_px(boot, x + col, y + row);
            }
        }
    }
}

fn restore_stamp<const N: usize>(boot: &CompositorBoot, stamp: &mut StampInner<N>) {
    if !stamp.live {
        return;
    }
    for row in 0..stamp.h {
        for col in 0..stamp.w {
            let index = (row * stamp.w + col) as usize;
            if index < N {
                write_px(boot, stamp.x + col, stamp.y + row, stamp.pixels[index]);
            }
        }
    }
    stamp.live = false;
    fence(Ordering::SeqCst);
}

fn overlaps(packed: u64, x: u32, y: u32, w: u32, h: u32) -> bool {
    let ox = (packed & 0xFFFF) as u32;
    let oy = ((packed >> 16) & 0xFFFF) as u32;
    let ow = ((packed >> 32) & 0xFFFF) as u32;
    let oh = ((packed >> 48) & 0xFFFF) as u32;
    ox < x.saturating_add(w) && x < ox.saturating_add(ow) && oy < y.saturating_add(h) && y < oy.saturating_add(oh)
}

fn read_px(boot: &CompositorBoot, x: u32, y: u32) -> u32 {
    let offset = y as usize * boot.pitch as usize + x as usize * 4;
    unsafe {
        (boot.fb as *const u8)
            .add(offset)
            .cast::<u32>()
            .read_volatile()
    }
}

fn write_px(boot: &CompositorBoot, x: u32, y: u32, pixel: u32) {
    let offset = y as usize * boot.pitch as usize + x as usize * 4;
    unsafe {
        (boot.fb as *mut u8)
            .add(offset)
            .cast::<u32>()
            .write_volatile(pixel);
    }
}

fn clear_screen(boot: &CompositorBoot) {
    let bg = encode(boot, DESKTOP_BG);
    let pitch = boot.pitch as usize;
    let fb = boot.fb as *mut u8;
    for y in 0..boot.height as usize {
        let row = unsafe { fb.add(y * pitch) };
        for x in 0..boot.width as usize {
            unsafe {
                row.add(x * 4).cast::<u32>().write_volatile(bg);
            }
        }
    }
    fence(Ordering::SeqCst);
}

fn paint_mark(boot: &CompositorBoot) {
    let (cx, cy, radius) = mark_geom(boot);
    meuxe_logo::for_each_pixel(radius, |dx, dy, rgb| {
        let x = cx + dx;
        let y = cy + dy;
        if x < 0 || y < 0 {
            return;
        }
        let x = x as u32;
        let y = y as u32;
        if x >= boot.width || y >= boot.height || reserved(x, y) {
            return;
        }
        let offset = y as usize * boot.pitch as usize + x as usize * 4;
        unsafe {
            (boot.fb as *mut u8)
                .add(offset)
                .cast::<u32>()
                .write_volatile(encode(boot, rgb));
        }
    });
    fence(Ordering::SeqCst);
}

fn reserved(x: u32, y: u32) -> bool {
    let window = x >= WINDOW_X
        && y >= WINDOW_Y
        && x < WINDOW_X + WINDOW_W
        && y < WINDOW_Y + WINDOW_H;
    let terminal = x >= TERM_X
        && y >= TERM_Y
        && x < TERM_X + TERM_PX_W
        && y < TERM_Y + TERM_PX_H;
    window
        || terminal
        || icon_hit(ICON_Y_TERM, x, y)
        || icon_hit(ICON_Y_FILES, x, y)
        || icon_hit(ICON_Y_CALC, x, y)
}

fn repair_mark(boot: &CompositorBoot, hole: Rect, desk: &Desktop) {
    let (cx, cy, radius) = mark_geom(boot);
    let term = outer(desk.term_rect());
    let files = outer(desk.files_rect());
    meuxe_logo::for_each_pixel(radius, |dx, dy, rgb| {
        let x = cx + dx;
        let y = cy + dy;
        if x < 0 || y < 0 {
            return;
        }
        let x = x as u32;
        let y = y as u32;
        if !point_in(hole, x, y) || x >= boot.width || y >= boot.height {
            return;
        }
        if desk.term_open && point_in(term, x, y) {
            return;
        }
        if desk.files_open && point_in(files, x, y) {
            return;
        }
        let calc = outer(desk.rect(FOCUS_CALC));
        if desk.calc_open && point_in(calc, x, y) {
            return;
        }
        write_px(boot, x, y, encode(boot, rgb));
    });
    fence(Ordering::SeqCst);
}

fn mark_geom(boot: &CompositorBoot) -> (i32, i32, i32) {
    let mut radius = 76i32;
    let cx = boot.width as i32 / 2;
    let mut cy = radius + 12;
    if cy + radius >= TERM_Y as i32 - 8 {
        radius = ((TERM_Y as i32 - 20) / 2).max(16);
        cy = radius + 8;
    }
    (cx, cy, radius)
}

fn covers_window(packed: u64) -> bool {
    let x = (packed & 0xFFFF) as u32;
    let y = ((packed >> 16) & 0xFFFF) as u32;
    let w = ((packed >> 32) & 0xFFFF) as u32;
    let h = ((packed >> 48) & 0xFFFF) as u32;
    x <= WINDOW_X && y <= WINDOW_Y && x + w >= WINDOW_X + WINDOW_W && y + h >= WINDOW_Y + WINDOW_H
}

fn post_recv(cap: u32, mailbox: u64, user_data: u64) {
    let recv = SubmissionEntry {
        opcode: SQ_OPCODE_RECV,
        flags: 0,
        cap,
        a: mailbox,
        b: 0,
        user_data,
    };
    let _ = ring().sq.push(recv);
    syscall(SYS_RING_PROCESS, 0, 0);
}

fn is_term(boot: &CompositorBoot, packed: u64) -> bool {
    inside(packed, boot.term_x, boot.term_y, TERM_PX_W, TERM_PX_H)
}

fn is_files(boot: &CompositorBoot, packed: u64) -> bool {
    boot.files != 0 && inside(packed, boot.files_x, boot.files_y, FILES_PX_W, FILES_PX_H)
}

fn is_calc(boot: &CompositorBoot, packed: u64) -> bool {
    boot.calc != 0 && inside(packed, boot.calc_x, boot.calc_y, CALC_PX_W, CALC_PX_H)
}

fn inside(packed: u64, ox: u32, oy: u32, ow: u32, oh: u32) -> bool {
    let x = (packed & 0xFFFF) as u32;
    let y = ((packed >> 16) & 0xFFFF) as u32;
    let w = ((packed >> 32) & 0xFFFF) as u32;
    let h = ((packed >> 48) & 0xFFFF) as u32;
    x >= ox && y >= oy && x.saturating_add(w) <= ox.saturating_add(ow) && y.saturating_add(h) <= oy.saturating_add(oh)
}

fn blit(boot: &CompositorBoot, packed: u64, desk: &Desktop) {
    let x = (packed & 0xFFFF) as u32;
    let y = ((packed >> 16) & 0xFFFF) as u32;
    let w = ((packed >> 32) & 0xFFFF) as u32;
    let h = ((packed >> 48) & 0xFFFF) as u32;
    if w == 0 || h == 0 {
        return;
    }
    let (base, stride, origin_x, origin_y, dest_x, dest_y) = if is_term(boot, packed) {
        let ox = x - boot.term_x;
        let oy = y - boot.term_y;
        (
            boot.term,
            boot.term_stride,
            ox,
            oy,
            desk.term_x + ox,
            desk.term_y + oy,
        )
    } else if is_files(boot, packed) {
        let ox = x - boot.files_x;
        let oy = y - boot.files_y;
        (
            boot.files,
            boot.files_stride,
            ox,
            oy,
            desk.files_x + ox,
            desk.files_y + oy,
        )
    } else if is_calc(boot, packed) {
        let ox = x - boot.calc_x;
        let oy = y - boot.calc_y;
        (
            boot.calc,
            boot.calc_stride,
            ox,
            oy,
            desk.calc_x + ox,
            desk.calc_y + oy,
        )
    } else {
        (boot.back, w, 0, 0, x, y)
    };
    blit_surface(boot, base, stride, dest_x, dest_y, w, h, origin_x, origin_y);
}

fn blit_windows(boot: &CompositorBoot, desk: &Desktop) {
    let front = order(desk);
    for kind in front.into_iter().rev() {
        if !desk.is_open(kind) {
            continue;
        }
        let rect = desk.rect(kind);
        let (base, stride) = match kind {
            FOCUS_FILES => (boot.files, boot.files_stride),
            FOCUS_CALC => (boot.calc, boot.calc_stride),
            _ => (boot.term, boot.term_stride),
        };
        blit_surface(boot, base, stride, rect.x, rect.y, rect.w, rect.h, 0, 0);
    }
}

fn blit_surface(
    boot: &CompositorBoot,
    base: u64,
    stride: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    origin_x: u32,
    origin_y: u32,
) {
    if base == 0 || w == 0 || h == 0 {
        return;
    }
    let src = base as *const u32;
    for row in 0..h {
        if y.saturating_add(row) >= boot.height {
            break;
        }
        for col in 0..w {
            if x.saturating_add(col) >= boot.width {
                break;
            }
            let index = (origin_y + row) as usize * stride as usize + (origin_x + col) as usize;
            let pixel = encode(boot, unsafe { src.add(index).read_volatile() });
            write_px(boot, x + col, y + row, pixel);
        }
    }
    fence(Ordering::SeqCst);
}

fn hit(boot: &CompositorBoot, rect: u64, pointer: u64) -> bool {
    let x = (rect & 0xFFFF) as u32;
    let y = ((rect >> 16) & 0xFFFF) as u32;
    let w = ((packed_w)(rect)) as u32;
    let h = ((rect >> 48) & 0xFFFF) as u32;
    let px = (pointer as u32) & !POINTER_HELD;
    let py = (pointer >> 32) as u32;
    let inside = px >= x && py >= y && px < x.saturating_add(w) && py < y.saturating_add(h);
    unsafe {
        let base = boot.status as *mut u32;
        base.add(1).write_volatile(px);
        base.add(2).write_volatile(py);
        fence(Ordering::SeqCst);
        base.write_volatile(if inside { 1 } else { 2 });
    }
    inside
}

fn packed_w(rect: u64) -> u32 {
    ((rect >> 32) & 0xFFFF) as u32
}

#[derive(Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

fn content_rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

fn close_hit(rect: Rect, px: u32, py: u32) -> bool {
    let y0 = rect.y.saturating_sub(TITLE_H);
    let x0 = rect.x.saturating_add(rect.w).saturating_sub(24);
    px >= x0 && px < rect.x.saturating_add(rect.w) && py >= y0 && py < rect.y
}

fn title_hit(rect: Rect, px: u32, py: u32) -> bool {
    let y0 = rect.y.saturating_sub(TITLE_H);
    let x1 = rect.x.saturating_add(rect.w).saturating_sub(24);
    px >= rect.x && px < x1 && py >= y0 && py < rect.y
}

fn outer(rect: Rect) -> Rect {
    Rect {
        x: rect.x.saturating_sub(FRAME_BORDER),
        y: rect.y.saturating_sub(TITLE_H + FRAME_BORDER),
        w: rect.w.saturating_add(FRAME_BORDER * 2 + 1),
        h: rect.h.saturating_add(TITLE_H + FRAME_BORDER * 2 + 1),
    }
}

fn point_in(rect: Rect, px: u32, py: u32) -> bool {
    px >= rect.x
        && py >= rect.y
        && px < rect.x.saturating_add(rect.w)
        && py < rect.y.saturating_add(rect.h)
}

fn in_outer(rect: Rect, px: u32, py: u32) -> bool {
    point_in(outer(rect), px, py)
}

fn fill_outer(boot: &CompositorBoot, rect: Rect) {
    let bounds = outer(rect);
    fill_rect(boot, bounds.x, bounds.y, bounds.w, bounds.h, DESKTOP_BG);
}

fn paint_frames(boot: &CompositorBoot, desk: &Desktop) {
    for kind in order(desk).into_iter().rev() {
        if !desk.is_open(kind) {
            continue;
        }
        paint_frame(
            boot,
            desk.rect(kind),
            pane_title(kind),
            kind == desk.focus,
        );
    }
}

fn pane_title(kind: u8) -> &'static [u8] {
    match kind {
        FOCUS_FILES => b"Files",
        FOCUS_CALC => b"Calc",
        _ => b"Terminal",
    }
}

fn icon_hit(icon_y: u32, px: u32, py: u32) -> bool {
    px >= ICON_X && px < ICON_X + ICON_HIT_W && py >= icon_y && py < icon_y + ICON_HIT_H
}

fn paint_icons(boot: &CompositorBoot) {
    paint_icon(boot, ICON_Y_TERM, b"Term", FOCUS_TERM);
    paint_icon(boot, ICON_Y_FILES, b"Files", FOCUS_FILES);
    paint_icon(boot, ICON_Y_CALC, b"Calc", FOCUS_CALC);
}

fn paint_icon(boot: &CompositorBoot, y: u32, label: &[u8], kind: u8) {
    fill_rect(boot, ICON_X, y, ICON_HIT_W, ICON_HIT_H, DESKTOP_BG);
    fill_rect(boot, ICON_X, y, ICON_BOX, ICON_BOX, theme::PANEL);
    fill_rect(boot, ICON_X, y, ICON_BOX, 1, FRAME_EDGE);
    fill_rect(boot, ICON_X, y + ICON_BOX - 1, ICON_BOX, 1, FRAME_EDGE);
    fill_rect(boot, ICON_X, y, 1, ICON_BOX, FRAME_EDGE);
    fill_rect(boot, ICON_X + ICON_BOX - 1, y, 1, ICON_BOX, FRAME_EDGE);
    if kind == FOCUS_TERM {
        fill_rect(boot, ICON_X + 5, y + 6, 22, 5, TITLE_BG);
        fill_rect(boot, ICON_X + 5, y + 11, 22, 14, theme::DISPLAY);
        fill_rect(boot, ICON_X + 5, y + 6, 22, 1, FRAME_ACCENT);
        draw_text(boot, ICON_X + 8, y + 14, b">_", 1, TITLE_FG);
    } else if kind == FOCUS_CALC {
        fill_rect(boot, ICON_X + 6, y + 8, 8, 6, TITLE_BG);
        fill_rect(boot, ICON_X + 16, y + 8, 8, 6, FRAME_ACCENT);
        fill_rect(boot, ICON_X + 6, y + 16, 8, 6, FRAME_ACCENT);
        fill_rect(boot, ICON_X + 16, y + 16, 8, 6, TITLE_BG);
    } else {
        fill_rect(boot, ICON_X + 6, y + 7, 10, 4, FRAME_ACCENT);
        fill_rect(boot, ICON_X + 6, y + 11, 20, 13, FRAME_ACCENT);
        fill_rect(boot, ICON_X + 8, y + 15, 16, 1, 0x00FF_E7C8);
        fill_rect(boot, ICON_X + 8, y + 18, 12, 1, 0x00FF_E7C8);
    }
    draw_text(boot, ICON_X, y + ICON_BOX + 2, label, 1, TITLE_FG);
}

fn paint_taskbar(boot: &CompositorBoot, desk: &Desktop) {
    let y = boot.height.saturating_sub(BAR_H);
    fill_rect(boot, 0, y, boot.width, 1, FRAME_EDGE);
    fill_rect(boot, 0, y + 1, boot.width, BAR_H - 1, TITLE_BG);
    draw_text(boot, 12, y + 6, b"meuxe", 2, TITLE_FG);
    for chip in chips(desk).into_iter().flatten() {
        let chip_y = y + 5;
        let fill = if chip.hot { 0x0030_3844 } else { 0x001A_1C24 };
        fill_rect(boot, chip.x, chip_y, chip.w, 18, fill);
        if chip.hot {
            fill_rect(boot, chip.x, chip_y, 2, 18, FRAME_ACCENT);
        }
        draw_text(boot, chip.x + 8, chip_y + 5, chip.label, 1, TITLE_FG);
    }
}

struct Chip {
    x: u32,
    w: u32,
    kind: u8,
    hot: bool,
    label: &'static [u8],
}

fn chips(desk: &Desktop) -> [Option<Chip>; 3] {
    let mut out = [None, None, None];
    let mut x = 108u32;
    let mut slot = 0usize;
    for kind in [FOCUS_TERM, FOCUS_FILES, FOCUS_CALC] {
        if !desk.is_open(kind) {
            continue;
        }
        let label = pane_title(kind);
        let w = 8 + label.len() as u32 * 8 + 8;
        out[slot] = Some(Chip {
            x,
            w,
            kind,
            hot: desk.focus == kind,
            label,
        });
        x += w + 8;
        slot += 1;
    }
    out
}

fn taskbar_hit(boot: &CompositorBoot, desk: &Desktop, px: u32, py: u32) -> Option<u8> {
    let y = boot.height.saturating_sub(BAR_H);
    if py < y + 5 || py >= y + 23 {
        return None;
    }
    for chip in chips(desk).into_iter().flatten() {
        if px >= chip.x && px < chip.x + chip.w {
            return Some(chip.kind);
        }
    }
    None
}

fn paint_frame(boot: &CompositorBoot, rect: Rect, title: &[u8], focused: bool) {
    let x = rect.x.saturating_sub(FRAME_BORDER);
    let y = rect.y.saturating_sub(TITLE_H + FRAME_BORDER);
    let w = rect.w.saturating_add(FRAME_BORDER * 2);
    let h = rect.h.saturating_add(TITLE_H + FRAME_BORDER * 2);
    fill_rect(boot, x + 1, y + h, w, 1, SHADOW);
    fill_rect(boot, x + w, y + 1, 1, h, SHADOW);
    fill_rect(boot, x, y, w, FRAME_BORDER, FRAME_EDGE);
    fill_rect(boot, x, y + h - FRAME_BORDER, w, FRAME_BORDER, FRAME_EDGE);
    fill_rect(boot, x, y, FRAME_BORDER, h, FRAME_EDGE);
    fill_rect(boot, x + w - FRAME_BORDER, y, FRAME_BORDER, h, FRAME_EDGE);
    let bar_x = rect.x;
    let bar_y = rect.y.saturating_sub(TITLE_H);
    fill_rect(boot, bar_x, bar_y, rect.w, TITLE_H, TITLE_BG);
    fill_rect(boot, bar_x, bar_y + TITLE_H - 1, rect.w, 1, FRAME_EDGE);
    if focused {
        fill_rect(boot, bar_x, bar_y, 3, TITLE_H, FRAME_ACCENT);
    }
    draw_text(boot, bar_x + 10, bar_y + 3, title, 2, TITLE_FG);
    let close_x = bar_x + rect.w - 20;
    fill_rect(boot, close_x, bar_y + 3, 16, 16, CLOSE_RGB);
    draw_text(boot, close_x + 4, bar_y + 7, b"x", 1, 0x00FF_FFFF);
}

fn draw_text(boot: &CompositorBoot, x: u32, y: u32, text: &[u8], scale: u32, rgb: u32) {
    let color = encode(boot, rgb);
    for (index, byte) in text.iter().enumerate() {
        let origin_x = x + index as u32 * 8 * scale;
        paint_glyph(*byte, scale, |dx, dy| {
            write_px(boot, origin_x + dx, y + dy, color);
        });
    }
}

fn note_calc(boot: &CompositorBoot, origin_x: u32, origin_y: u32, px: u32, py: u32) {
    if boot.calc_pick == 0
        || px < origin_x
        || py < origin_y
        || px >= origin_x.saturating_add(CALC_PX_W)
        || py >= origin_y.saturating_add(CALC_PX_H)
    {
        return;
    }
    unsafe {
        let base = boot.calc_pick as *mut u32;
        let seq = base.read_volatile().wrapping_add(1).max(1);
        base.add(1).write_volatile(px - origin_x);
        base.add(2).write_volatile(py - origin_y);
        fence(Ordering::SeqCst);
        base.write_volatile(seq);
    }
}

fn note_pick(boot: &CompositorBoot, origin_x: u32, origin_y: u32, px: u32, py: u32) {
    if boot.pick == 0
        || px < origin_x
        || py < origin_y
        || px >= origin_x.saturating_add(FILES_PX_W)
        || py >= origin_y.saturating_add(FILES_PX_H)
    {
        return;
    }
    unsafe {
        let base = boot.pick as *mut u32;
        let seq = base.read_volatile().wrapping_add(1).max(1);
        base.add(1).write_volatile(px - origin_x);
        base.add(2).write_volatile(py - origin_y);
        fence(Ordering::SeqCst);
        base.write_volatile(seq);
    }
}

fn encode(boot: &CompositorBoot, rgb: u32) -> u32 {
    let red = (rgb >> 16) & 0xFF;
    let green = (rgb >> 8) & 0xFF;
    let blue = rgb & 0xFF;
    (red << boot.red_shift) | (green << boot.green_shift) | (blue << boot.blue_shift)
}
