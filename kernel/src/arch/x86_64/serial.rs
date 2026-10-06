//! COM1, 115200 8n1. This is the boot log until the framebuffer console is up.

use super::cpu;
use core::fmt;

const PORT: u16 = 0x3F8;

pub fn init() {
    cpu::outb(PORT + 1, 0x00);
    cpu::outb(PORT + 3, 0x80);
    cpu::outb(PORT + 0, 0x01);
    cpu::outb(PORT + 1, 0x00);
    cpu::outb(PORT + 3, 0x03);
    cpu::outb(PORT + 2, 0xC7);
    cpu::outb(PORT + 4, 0x0B);
}

pub struct Writer;

impl fmt::Write for Writer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' {
                write_byte(b'\r');
            }
            write_byte(byte);
        }
        Ok(())
    }
}

fn write_byte(byte: u8) {
    let mut spins = 0;
    while cpu::inb(PORT + 5) & 0x20 == 0 {
        spins += 1;
        if spins > 100_000 {
            break;
        }
        core::hint::spin_loop();
    }
    cpu::outb(PORT, byte);
}
