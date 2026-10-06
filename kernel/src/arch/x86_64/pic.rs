//! Legacy dual 8259. Limine already masks both controllers; we mask them again
//! so a later GDT/IDT reload cannot observe a live PIC line.

use super::cpu;

pub fn disable() {
    cpu::outb(0x21, 0xFF);
    cpu::outb(0xA1, 0xFF);
}
