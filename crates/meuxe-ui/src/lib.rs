//! Toolkit for programs that draw a window on the Meuxe desktop.
//!
//! [`Canvas`] writes pixels into the window buffer. [`damage`] tells the
//! compositor which screen rectangle changed. [`key_of`] turns a virtio key
//! code into a character. With the `app` feature, [`Dir`] lists, reads, and
//! writes named records through the directory endpoint.

#![no_std]

use meuxe_font::{glyph, paint_glyph};

/// A window's pixel buffer. Coordinates are inside the window, not the screen.
pub struct Canvas {
    pixels: *mut u32,
    stride: u32,
    width: u32,
    height: u32,
}

impl Canvas {
    pub fn new(pixels: *mut u32, stride: u32, width: u32, height: u32) -> Self {
        Self {
            pixels,
            stride,
            width,
            height,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn fill(&self, rgb: u32) {
        self.rect(0, 0, self.width, self.height, rgb);
    }

    pub fn put(&self, x: u32, y: u32, rgb: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        unsafe {
            self.pixels
                .add((y * self.stride + x) as usize)
                .write_volatile(rgb);
        }
    }

    pub fn rect(&self, x: u32, y: u32, w: u32, h: u32, rgb: u32) {
        for row in 0..h {
            for col in 0..w {
                self.put(x.saturating_add(col), y.saturating_add(row), rgb);
            }
        }
    }

    /// One-pixel frame. The interior is left alone.
    pub fn stroke(&self, x: u32, y: u32, w: u32, h: u32, rgb: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.rect(x, y, w, 1, rgb);
        self.rect(x, y.saturating_add(h - 1), w, 1, rgb);
        self.rect(x, y, 1, h, rgb);
        self.rect(x.saturating_add(w - 1), y, 1, h, rgb);
    }

    /// Draw one cell. Every pixel is written: ink in `fg`, the rest in `bg`.
    /// `caret` replaces the bottom row of the cell when that pixel is not ink.
    pub fn glyph(
        &self,
        x: u32,
        y: u32,
        ch: u8,
        scale: u32,
        fg: u32,
        bg: u32,
        caret: Option<u32>,
    ) {
        let scale = scale.max(1);
        let bits = glyph(ch);
        for gy in 0..8u32 {
            let row_bits = bits[gy as usize];
            for gx in 0..8u32 {
                let lit = row_bits & (1 << gx) != 0;
                let pixel = if lit {
                    fg
                } else if caret.is_some() && gy >= 7 {
                    caret.unwrap()
                } else {
                    bg
                };
                for sy in 0..scale {
                    for sx in 0..scale {
                        self.put(
                            x + gx * scale + sx,
                            y + gy * scale + sy,
                            pixel,
                        );
                    }
                }
            }
        }
    }

    /// Ink only. The caller has already filled the background.
    pub fn text(&self, x: u32, y: u32, bytes: &[u8], scale: u32, fg: u32) {
        let scale = scale.max(1);
        for (index, byte) in bytes.iter().enumerate() {
            let origin = x + index as u32 * 8 * scale;
            paint_glyph(*byte, scale, |dx, dy| {
                self.put(origin + dx, y + dy, fg);
            });
        }
    }

    /// Place a string on the character grid, filling each cell's background.
    pub fn cells(&self, col: u32, row: u32, bytes: &[u8], scale: u32, fg: u32, bg: u32) {
        let scale = scale.max(1);
        let cell = 8 * scale;
        for (index, byte) in bytes.iter().enumerate() {
            self.glyph(
                (col + index as u32) * cell,
                row * cell,
                *byte,
                scale,
                fg,
                bg,
                None,
            );
        }
    }
}

/// Virtio key codes the shell and other apps share.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(u8),
    Enter,
    Backspace,
}

pub fn key_of(code: u16) -> Option<Key> {
    match code {
        14 => Some(Key::Backspace),
        28 => Some(Key::Enter),
        57 => Some(Key::Char(b' ')),
        16..=25 => Some(Key::Char(b"qwertyuiop"[(code - 16) as usize])),
        30..=38 => Some(Key::Char(b"asdfghjkl"[(code - 30) as usize])),
        44..=50 => Some(Key::Char(b"zxcvbnm"[(code - 44) as usize])),
        _ => None,
    }
}

#[cfg(feature = "app")]
mod app {
    use core::sync::atomic::{fence, Ordering};
    use meuxe_abi::{pack_rect, SubmissionEntry, ERR_AGAIN, RESULT_OK, SYS_RING_PROCESS, SQ_OPCODE_SEND};
    use meuxe_rt::{ring, syscall, yield_once};

    const SEND_TAG: u64 = 3;
    const OP_LIST: u32 = 1;
    const OP_READ: u32 = 2;
    const OP_WRITE: u32 = 3;

    /// Publish a dirty rectangle in screen coordinates. The compositor adds
    /// any distance the window has been dragged.
    pub fn damage(cap: u32, x: u32, y: u32, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        let packed = pack_rect(x, y, w, h);
        loop {
            let send = SubmissionEntry {
                opcode: SQ_OPCODE_SEND,
                flags: 0,
                cap,
                a: packed,
                b: 0,
                user_data: SEND_TAG,
            };
            if ring().sq.push(send).is_err() {
                yield_once();
                continue;
            }
            syscall(SYS_RING_PROCESS, 0, 0);
            loop {
                match completion(SEND_TAG) {
                    Some(RESULT_OK) => return,
                    Some(ERR_AGAIN) => break,
                    Some(_) => return,
                    None => yield_once(),
                }
            }
        }
    }

    /// One directory endpoint and the shared request page behind it.
    pub struct Dir {
        pub page: u64,
        pub cap: u32,
    }

    impl Dir {
        pub fn list(&self, out: &mut [u8]) -> usize {
            self.call(OP_LIST, b"", b"", out)
        }

        pub fn read(&self, name: &[u8], out: &mut [u8]) -> usize {
            self.call(OP_READ, name, b"", out)
        }

        pub fn write(&self, name: &[u8], data: &[u8], out: &mut [u8]) -> usize {
            self.call(OP_WRITE, name, data, out)
        }

        fn call(&self, op: u32, name: &[u8], data: &[u8], out: &mut [u8]) -> usize {
            let name_len = name.len().min(16);
            let data_len = data.len().min(32);
            unsafe {
                (self.page as *mut u32).write_volatile(op);
                ((self.page + 4) as *mut u32).write_volatile(name_len as u32);
                let slot = (self.page + 8) as *mut u8;
                for index in 0..16 {
                    let byte = if index < name_len { name[index] } else { 0 };
                    slot.add(index).write_volatile(byte);
                }
                ((self.page + 80) as *mut u32).write_volatile(data_len as u32);
                let body = (self.page + 84) as *mut u8;
                for index in 0..32 {
                    let byte = if index < data_len { data[index] } else { 0 };
                    body.add(index).write_volatile(byte);
                }
                ((self.page + 24) as *mut u32).write_volatile(0);
                ((self.page + 28) as *mut u32).write_volatile(0);
            }
            fence(Ordering::SeqCst);
            if !self.send() {
                return 0;
            }
            for _ in 0..80_000 {
                let status = unsafe { ((self.page + 24) as *const u32).read_volatile() };
                if status != 0 {
                    let len = unsafe { ((self.page + 28) as *const u32).read_volatile() } as usize;
                    let len = len.min(out.len());
                    let src = (self.page + 32) as *const u8;
                    for index in 0..len {
                        out[index] = unsafe { src.add(index).read_volatile() };
                    }
                    return len;
                }
                yield_once();
            }
            0
        }

        fn send(&self) -> bool {
            for _ in 0..8 {
                let send = SubmissionEntry {
                    opcode: SQ_OPCODE_SEND,
                    flags: 0,
                    cap: self.cap,
                    a: 7,
                    b: 0,
                    user_data: 7,
                };
                if ring().sq.push(send).is_err() {
                    yield_once();
                    continue;
                }
                syscall(SYS_RING_PROCESS, 0, 0);
                loop {
                    match completion(7) {
                        Some(RESULT_OK) => return true,
                        Some(ERR_AGAIN) => break,
                        Some(_) => return false,
                        None => yield_once(),
                    }
                }
            }
            false
        }
    }

    /// Sequence number of the latest click inside a window. Zero means none yet.
    pub fn pick_seq(page: u64) -> u32 {
        if page == 0 {
            return 0;
        }
        unsafe { (page as *const u32).read_volatile() }
    }

    /// Click position relative to the window's content origin.
    pub fn pick_at(page: u64) -> (u32, u32) {
        if page == 0 {
            return (0, 0);
        }
        unsafe {
            let base = page as *const u32;
            (base.add(1).read_volatile(), base.add(2).read_volatile())
        }
    }

    fn completion(tag: u64) -> Option<i32> {
        let mut found = None;
        while let Some(entry) = ring().cq.pop() {
            if entry.user_data == tag {
                found = Some(entry.result);
            }
        }
        found
    }
}

#[cfg(feature = "app")]
pub use app::{damage, pick_at, pick_seq, Dir};

/// Colors shared by the desktop chrome and the apps.
pub mod theme {
    pub const DESKTOP: u32 = 0x0012_141A;
    pub const INK: u32 = 0x00E7_E1D5;
    pub const ACCENT: u32 = 0x00E0_7A3D;
    pub const PANEL: u32 = 0x001A_1C24;
    pub const TITLE: u32 = 0x0024_2B36;
    pub const EDGE: u32 = 0x003C_4450;
    pub const CLOSE: u32 = 0x00C2_1858;
    pub const SHADOW: u32 = 0x0006_080C;
    pub const DISPLAY: u32 = 0x0010_141C;
}

/// Pending calculator operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CalcOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// One press on the calculator keypad.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CalcKey {
    Digit(u8),
    Op(CalcOp),
    Eq,
    Clear,
}

/// Integer calculator. Division truncates toward zero. Overflow and division
/// by zero show as an error until clear.
#[derive(Clone, Copy, Debug)]
pub struct Calc {
    acc: i64,
    entry: i64,
    op: Option<CalcOp>,
    fresh: bool,
    err: bool,
}

impl Calc {
    pub const fn new() -> Self {
        Self {
            acc: 0,
            entry: 0,
            op: None,
            fresh: true,
            err: false,
        }
    }

    pub fn press(&mut self, key: CalcKey) {
        match key {
            CalcKey::Clear => *self = Self::new(),
            CalcKey::Digit(digit) => self.digit(digit),
            CalcKey::Op(op) => self.operate(op),
            CalcKey::Eq => self.equals(),
        }
    }

    pub fn text(&self, out: &mut [u8]) -> usize {
        if self.err {
            return copy_bytes(b"err", out);
        }
        format_i64(self.entry, out)
    }

    fn digit(&mut self, digit: u8) {
        if self.err {
            return;
        }
        let digit = (digit % 10) as i64;
        if self.fresh {
            self.entry = digit;
            self.fresh = false;
            return;
        }
        let next = self.entry.checked_mul(10).and_then(|value| {
            if self.entry < 0 {
                value.checked_sub(digit)
            } else {
                value.checked_add(digit)
            }
        });
        match next {
            Some(value) => self.entry = value,
            None => self.err = true,
        }
    }

    fn operate(&mut self, op: CalcOp) {
        if self.err {
            return;
        }
        if self.op.is_some() && !self.fresh {
            if !self.apply() {
                return;
            }
        } else if self.op.is_none() {
            self.acc = self.entry;
        }
        self.op = Some(op);
        self.fresh = true;
    }

    fn equals(&mut self) {
        if self.err {
            return;
        }
        if self.op.is_some() && !self.apply() {
            return;
        }
        self.op = None;
        self.fresh = true;
    }

    fn apply(&mut self) -> bool {
        let result = match self.op {
            Some(CalcOp::Add) => self.acc.checked_add(self.entry),
            Some(CalcOp::Sub) => self.acc.checked_sub(self.entry),
            Some(CalcOp::Mul) => self.acc.checked_mul(self.entry),
            Some(CalcOp::Div) if self.entry != 0 => self.acc.checked_div(self.entry),
            Some(CalcOp::Div) => None,
            None => Some(self.entry),
        };
        match result {
            Some(value) => {
                self.acc = value;
                self.entry = value;
                true
            }
            None => {
                self.err = true;
                false
            }
        }
    }
}

/// Key under a point inside the calculator surface. `scale` is the cell scale.
pub fn calc_key_at(local_x: u32, local_y: u32, scale: u32) -> Option<CalcKey> {
    let cell = 8 * scale.max(1);
    let col = local_x / cell;
    let row = local_y / cell;
    if !(4..=11).contains(&row) {
        return None;
    }
    let grid_row = (row - 4) / 2;
    let grid_col = if (1..5).contains(&col) {
        0
    } else if (6..10).contains(&col) {
        1
    } else if (11..15).contains(&col) {
        2
    } else if (16..20).contains(&col) {
        3
    } else {
        return None;
    };
    const KEYS: [CalcKey; 16] = [
        CalcKey::Digit(7),
        CalcKey::Digit(8),
        CalcKey::Digit(9),
        CalcKey::Op(CalcOp::Div),
        CalcKey::Digit(4),
        CalcKey::Digit(5),
        CalcKey::Digit(6),
        CalcKey::Op(CalcOp::Mul),
        CalcKey::Digit(1),
        CalcKey::Digit(2),
        CalcKey::Digit(3),
        CalcKey::Op(CalcOp::Sub),
        CalcKey::Digit(0),
        CalcKey::Clear,
        CalcKey::Eq,
        CalcKey::Op(CalcOp::Add),
    ];
    KEYS.get((grid_row * 4 + grid_col) as usize).copied()
}

fn copy_bytes(src: &[u8], out: &mut [u8]) -> usize {
    let n = src.len().min(out.len());
    out[..n].copy_from_slice(&src[..n]);
    n
}

fn format_i64(value: i64, out: &mut [u8]) -> usize {
    if out.is_empty() {
        return 0;
    }
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    let neg = value < 0;
    let mut rest = if value == i64::MIN {
        // One past the positive range. Spell it out.
        return copy_bytes(b"-9223372036854775808", out);
    } else if neg {
        value.wrapping_neg() as u64
    } else {
        value as u64
    };
    let mut digits = [0u8; 20];
    let mut n = 0usize;
    while rest > 0 {
        digits[n] = b'0' + (rest % 10) as u8;
        n += 1;
        rest /= 10;
    }
    let mut written = 0usize;
    if neg && written < out.len() {
        out[written] = b'-';
        written += 1;
    }
    while n > 0 && written < out.len() {
        n -= 1;
        out[written] = digits[n];
        written += 1;
    }
    written
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::{calc_key_at, key_of, Calc, CalcKey, CalcOp, Canvas, Key};
    use meuxe_font::glyph;

    #[test]
    fn glyph_matches_the_font_and_paints_a_caret() {
        let mut buf = [0u32; 16 * 16];
        let canvas = Canvas::new(buf.as_mut_ptr(), 16, 16, 16);
        canvas.glyph(0, 0, b'h', 1, 0x11, 0x22, Some(0x33));
        let bits = glyph(b'h');
        for gy in 0..8u32 {
            for gx in 0..8u32 {
                let lit = bits[gy as usize] & (1 << gx) != 0;
                let pixel = buf[(gy * 16 + gx) as usize];
                if lit {
                    assert_eq!(pixel, 0x11);
                } else if gy >= 7 {
                    assert_eq!(pixel, 0x33);
                } else {
                    assert_eq!(pixel, 0x22);
                }
            }
        }
    }

    #[test]
    fn text_leaves_the_background() {
        let mut buf = [7u32; 16 * 8];
        let canvas = Canvas::new(buf.as_mut_ptr(), 16, 16, 8);
        canvas.text(0, 0, b" ", 1, 0x11);
        assert!(buf.iter().all(|pixel| *pixel == 7));
    }

    #[test]
    fn stroke_is_a_one_pixel_frame() {
        let mut buf = [0u32; 5 * 5];
        let canvas = Canvas::new(buf.as_mut_ptr(), 5, 5, 5);
        canvas.stroke(0, 0, 5, 5, 1);
        assert_eq!(buf[0], 1);
        assert_eq!(buf[6], 0);
        assert_eq!(buf[24], 1);
    }

    #[test]
    fn keys_follow_the_qwerty_rows() {
        assert_eq!(key_of(16), Some(Key::Char(b'q')));
        assert_eq!(key_of(30), Some(Key::Char(b'a')));
        assert_eq!(key_of(44), Some(Key::Char(b'z')));
        assert_eq!(key_of(57), Some(Key::Char(b' ')));
        assert_eq!(key_of(28), Some(Key::Enter));
        assert_eq!(key_of(14), Some(Key::Backspace));
        assert_eq!(key_of(1), None);
    }

    fn show(calc: &Calc) -> std::string::String {
        let mut buf = [0u8; 24];
        let n = calc.text(&mut buf);
        std::string::String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    #[test]
    fn calculator_adds_and_clears() {
        let mut calc = Calc::new();
        calc.press(CalcKey::Digit(1));
        calc.press(CalcKey::Digit(2));
        calc.press(CalcKey::Op(CalcOp::Add));
        calc.press(CalcKey::Digit(3));
        calc.press(CalcKey::Digit(0));
        calc.press(CalcKey::Eq);
        assert_eq!(show(&calc), "42");
        calc.press(CalcKey::Clear);
        assert_eq!(show(&calc), "0");
    }

    #[test]
    fn calculator_multiplies_subtracts_and_rejects_divide_by_zero() {
        let mut calc = Calc::new();
        calc.press(CalcKey::Digit(9));
        calc.press(CalcKey::Op(CalcOp::Mul));
        calc.press(CalcKey::Digit(9));
        calc.press(CalcKey::Eq);
        assert_eq!(show(&calc), "81");
        calc.press(CalcKey::Op(CalcOp::Sub));
        calc.press(CalcKey::Digit(3));
        calc.press(CalcKey::Eq);
        assert_eq!(show(&calc), "78");
        calc.press(CalcKey::Op(CalcOp::Div));
        calc.press(CalcKey::Digit(0));
        calc.press(CalcKey::Eq);
        assert_eq!(show(&calc), "err");
    }

    #[test]
    fn calculator_keys_follow_the_pad() {
        assert_eq!(calc_key_at(16, 64, 2), Some(CalcKey::Digit(7)));
        assert_eq!(calc_key_at(80, 64, 2), None);
        assert_eq!(calc_key_at(96, 64, 2), Some(CalcKey::Digit(8)));
        assert_eq!(calc_key_at(16 * 12, 16 * 10, 2), Some(CalcKey::Eq));
        assert_eq!(calc_key_at(16 * 18, 16 * 4, 2), Some(CalcKey::Op(CalcOp::Div)));
    }
}
