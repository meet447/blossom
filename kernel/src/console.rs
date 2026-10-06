//! Early framebuffer console. The boot mark is a blossom; log lines after
//! [`release`] stay on the serial port so they do not cover it.

use crate::boot::FbInfo;
use crate::font;
use crate::sync::Mutex;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

const BG: u32 = 0x12141A;
const INK: u32 = 0xE7E1D5;
const TITLE: u32 = 0xF3E6C8;

struct Console {
    fb: FbInfo,
    column: u32,
    row: u32,
    origin_y: u32,
}

static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);
static RELEASED: AtomicBool = AtomicBool::new(false);

/// The compositor owns the framebuffer after this. Later log lines stay on serial.
pub fn release() {
    RELEASED.store(true, Ordering::Release);
}

pub fn init(fb: FbInfo) {
    if fb.bpp != 32 {
        return;
    }
    let console = Console {
        fb,
        column: 2,
        row: 0,
        origin_y: 88,
    };
    console.fill(BG);
    *CONSOLE.lock() = Some(console);
}

/// Full-screen blossom and the meuxe wordmark. Replaces whatever the pixel
/// probe wrote into the corner.
pub fn splash() {
    let guard = CONSOLE.lock();
    let Some(console) = guard.as_ref() else {
        return;
    };
    console.fill(BG);
    let width = console.fb.width as i32;
    let height = console.fb.height as i32;
    if width < 32 || height < 32 {
        return;
    }
    let radius = (width.min(height) * 28 / 100).clamp(32, 240);
    let cx = width / 2;
    let cy = height / 2 - radius / 8;
    meuxe_logo::for_each_pixel(radius, |dx, dy, rgb| {
        let x = cx + dx;
        let y = cy + dy;
        if x >= 0 && y >= 0 {
            console.pixel(x as u32, y as u32, rgb);
        }
    });
    let scale = if height > 600 { 3 } else { 2 };
    let label = "meuxe";
    let advance = 8 * scale + scale;
    let text_w = label.len() as u32 * advance - scale;
    let x = (console.fb.width / 2).saturating_sub(text_w / 2);
    let y = (cy as u32)
        .saturating_add(radius as u32)
        .saturating_add(16);
    if y + 8 * scale < console.fb.height {
        console.text(label, x, y, scale, TITLE);
    }
}

pub fn write_fmt(args: fmt::Arguments) {
    if RELEASED.load(Ordering::Acquire) {
        return;
    }
    if let Some(console) = CONSOLE.lock().as_mut() {
        let _ = fmt::Write::write_fmt(console, args);
    }
}

pub fn poke(x: u32, y: u32, rgb: u32) -> Option<u32> {
    let mut guard = CONSOLE.lock();
    let console = guard.as_mut()?;
    let encoded = console.encode(rgb);
    console.raw(x, y, encoded);
    Some(encoded)
}

pub fn peek(x: u32, y: u32) -> Option<u32> {
    let guard = CONSOLE.lock();
    let console = guard.as_ref()?;
    console.read(x, y)
}

impl Console {
    fn encode(&self, rgb: u32) -> u32 {
        let red = (rgb >> 16) & 0xFF;
        let green = (rgb >> 8) & 0xFF;
        let blue = rgb & 0xFF;
        (red << self.fb.red_shift) | (green << self.fb.green_shift) | (blue << self.fb.blue_shift)
    }

    fn raw(&self, x: u32, y: u32, encoded: u32) {
        if x >= self.fb.width || y >= self.fb.height {
            return;
        }
        let offset = y as usize * self.fb.pitch as usize + x as usize * 4;
        unsafe {
            (self.fb.virt as *mut u8)
                .add(offset)
                .cast::<u32>()
                .write_volatile(encoded);
        }
    }

    fn read(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.fb.width || y >= self.fb.height {
            return None;
        }
        let offset = y as usize * self.fb.pitch as usize + x as usize * 4;
        Some(unsafe {
            (self.fb.virt as *const u8)
                .add(offset)
                .cast::<u32>()
                .read_volatile()
        })
    }

    fn pixel(&self, x: u32, y: u32, rgb: u32) {
        self.raw(x, y, self.encode(rgb));
    }

    fn fill(&self, rgb: u32) {
        let encoded = self.encode(rgb);
        for y in 0..self.fb.height {
            for x in 0..self.fb.width {
                self.raw(x, y, encoded);
            }
        }
    }

    fn text(&self, text: &str, mut x: u32, y: u32, scale: u32, rgb: u32) {
        for byte in text.bytes() {
            self.glyph(byte, x, y, scale, rgb);
            x = x.saturating_add(8 * scale + scale);
        }
    }

    fn glyph(&self, ch: u8, x: u32, y: u32, scale: u32, rgb: u32) {
        let glyph = font::glyph(ch);
        for row in 0..8u32 {
            let bits = glyph[row as usize];
            for col in 0..8u32 {
                if bits & (1 << col) == 0 {
                    continue;
                }
                for sy in 0..scale {
                    for sx in 0..scale {
                        self.pixel(x + col * scale + sx, y + row * scale + sy, rgb);
                    }
                }
            }
        }
    }

    fn newline(&mut self) {
        self.column = 2;
        let next = self.row.saturating_add(1);
        let line = 16;
        let y = self.origin_y + next * line;
        if y + line >= self.fb.height {
            self.scroll();
        } else {
            self.row = next;
        }
    }

    fn scroll(&mut self) {
        let line = 16u32;
        let start = self.origin_y;
        let bytes = self.fb.pitch as usize;
        for y in start..self.fb.height.saturating_sub(line) {
            unsafe {
                let dst = (self.fb.virt as *mut u8).add(y as usize * bytes);
                let src = (self.fb.virt as *const u8).add((y + line) as usize * bytes);
                core::ptr::copy(src, dst, bytes);
            }
        }
        let encoded = self.encode(BG);
        for y in self.fb.height.saturating_sub(line)..self.fb.height {
            for x in 0..self.fb.width {
                self.raw(x, y, encoded);
            }
        }
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' || byte == b'\r' {
                self.newline();
                continue;
            }
            let x = 8 + self.column * 16;
            if x + 16 >= self.fb.width {
                self.newline();
            }
            let x = 8 + self.column * 16;
            let y = self.origin_y + self.row * 16;
            self.glyph(byte, x, y, 2, INK);
            self.column += 1;
        }
        Ok(())
    }
}
