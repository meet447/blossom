//! x86_64 bring-up: CPU control, descriptors, and interrupt controllers.

pub mod apic;
pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod ioapic;
pub mod percpu;
pub mod pic;
pub mod serial;
pub mod smp;
pub mod syscall;

pub use cpu::{debug_exit, hlt, halt_forever};
