//! Devices the kernel programs before a server runs.
//!
//! `pci` finds virtio MMIO windows and arms MSI-X. `irq` records completions
//! for vectors 33 (block), 34 (tablet), and 35 (keyboard). The handler does
//! not allocate, log, or take a lock. `SYS_WAIT_IRQ` prints the line.

pub mod irq;
pub mod pci;
