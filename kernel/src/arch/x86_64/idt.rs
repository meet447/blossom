//! Interrupt descriptor table.
//!
//! Stubs in `isr.S` normalize every vector to a SysV frame so the Rust
//! handler does not depend on the `x86-interrupt` argument layout.

use super::cpu;
use crate::log;
use crate::sched;
use crate::task::{DYN_FIRST, DYN_LAST};
use super::percpu;
use core::arch::{asm, global_asm};
use core::mem::size_of;

global_asm!(include_str!("isr.S"));

#[repr(C)]
pub struct Frame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl IdtEntry {
    const fn empty() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            zero: 0,
        }
    }

    fn set(&mut self, handler: u64, ist: u8) {
        self.offset_low = handler as u16;
        self.selector = 0x08;
        self.ist = ist & 0x7;
        self.type_attr = 0x8E;
        self.offset_mid = (handler >> 16) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.zero = 0;
    }
}

#[repr(C, align(16))]
struct Idt {
    entries: [IdtEntry; 256],
}

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

static mut IDT: Idt = Idt {
    entries: [IdtEntry::empty(); 256],
};

extern "C" {
    static isr_table: [*const (); 256];
}

pub fn init() {
    unsafe {
        let table = core::ptr::addr_of!(isr_table) as *const *const ();
        for vector in 0..256 {
            let handler = table.add(vector).read() as u64;
            let ist = if vector == 8 { 1 } else { 0 };
            IDT.entries[vector].set(handler, ist);
        }
    }
    load();
}

pub fn load() {
    unsafe {
        let pointer = DescriptorPointer {
            limit: (size_of::<Idt>() - 1) as u16,
            base: core::ptr::addr_of!(IDT) as u64,
        };
        asm!("lidt [{ptr}]", ptr = in(reg) &pointer, options(readonly));
    }
}

#[no_mangle]
extern "C" fn rust_interrupt(frame: *mut Frame) -> *mut Frame {
    let view = unsafe { &*frame };
    if view.vector == 32 {
        crate::dev::irq::signal(32);
        let next = sched::preempt(frame);
        super::apic::eoi();
        return next;
    }
    if (33..=47).contains(&view.vector) {
        crate::dev::irq::signal(view.vector as u8);
        super::apic::eoi();
        return frame;
    }
    if view.vector == 0xFF {
        return frame;
    }
    let cr2 = if view.vector == 14 {
        cpu::read_cr2()
    } else {
        0
    };
    let current = unsafe { (*percpu::this()).current_task as u8 };
    if current >= DYN_FIRST && current <= DYN_LAST {
        crate::proc::kill_fault(current, view.vector, cr2);
        return sched::preempt(frame);
    }
    log::fault(view.vector, view.error, view.rip, cr2);
    if cfg!(feature = "verify") {
        cpu::debug_exit(0x03);
    }
    cpu::halt_forever();
}
