//! Devices the kernel programs before a server runs.
//!
//! `pci` finds virtio MMIO windows and arms MSI-X. `irq` records completions
//! for vectors 32 through 47. The handler does not allocate, log, or take a
//! lock. `SYS_WAIT_IRQ` prints the device line.

pub mod irq;
pub mod pci;
