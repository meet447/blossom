//! Boot order for the alpha kernel.
//!
//! `machine` installs the kernel's own page tables and stops at `boot ready`.
//! `sched` proves capabilities, the second CPU, and the ring-3 stub.
//! `service` then starts the userspace servers. `verify` samples the
//! framebuffer only when the `verify` feature is on.

mod machine;
mod sched;

use crate::arch::x86_64::cpu;

const WRITABLE: u64 = 1 << 1;
const NX: u64 = 1 << 63;

pub fn run() -> ! {
    let boot = machine::bringup();
    if let Err(error) = sched::bringup(&boot) {
        fail(error);
    }
    crate::kprintln!("meuxe: sched ready");
    if let Err(error) = crate::service::start(&boot) {
        fail(error);
    }
    if cfg!(feature = "verify") {
        if let Err(error) = crate::verify::finish(&boot) {
            fail(error);
        }
        cpu::debug_exit(0x10);
    }
    loop {
        cpu::hlt();
    }
}

pub(crate) fn check_permissions() {
    let (text, text_end) = crate::mm::text();
    let (data, data_end) = crate::mm::data();
    if text >= text_end {
        fail("kernel text is empty");
    }
    let text_flags = crate::mm::leaf_flags(text).unwrap_or_else(|| fail("text page is not mapped"));
    crate::kprintln!("meuxe: pte text={text_flags:#x}");
    if text_flags & WRITABLE != 0 || text_flags & NX != 0 {
        fail("kernel text is not read-execute");
    }
    if data >= data_end {
        fail("kernel data is empty");
    }
    let data_flags = crate::mm::leaf_flags(data).unwrap_or_else(|| fail("data page is not mapped"));
    crate::kprintln!("meuxe: pte data={data_flags:#x}");
    if data_flags & WRITABLE == 0 || data_flags & NX == 0 {
        fail("kernel data is not read-write no-execute");
    }
    let (rodata, rodata_end) = crate::mm::rodata();
    if rodata < rodata_end {
        let ro_flags =
            crate::mm::leaf_flags(rodata).unwrap_or_else(|| fail("rodata page is not mapped"));
        crate::kprintln!("meuxe: pte rodata={ro_flags:#x}");
        if ro_flags & WRITABLE != 0 || ro_flags & NX == 0 {
            fail("kernel rodata is not read-only no-execute");
        }
    }
    let heap_flags =
        crate::mm::leaf_flags(crate::mm::HEAP_VIRT).unwrap_or_else(|| fail("heap is not mapped"));
    crate::kprintln!("meuxe: pte heap={heap_flags:#x}");
    if heap_flags & WRITABLE == 0 || heap_flags & NX == 0 {
        fail("kernel heap is not read-write no-execute");
    }
}

pub fn fail(msg: &str) -> ! {
    crate::kprintln!("meuxe: fatal: {msg}");
    if cfg!(feature = "verify") {
        cpu::debug_exit(0x01);
    }
    cpu::halt_forever();
}
