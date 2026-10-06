//! Task ids for the alpha kernel.
//!
//! The scheduler table holds [`MAX`] tasks, ids 0 through 15. An application
//! processor's idle thread uses the task id equal to its CPU index, so a
//! two-CPU boot occupies task 1. Task [`MINT_TARGET`] exists so the capability
//! proof can mint into it. Nothing is scheduled there.
//!
//! Free ids on a two-CPU boot: 3, 5, 6, and 7. Do not add a seventeenth
//! task. Extra CPUs would claim their own index as an idle thread, which
//! collides with the calculator at id 2, so this alpha stays at two CPUs.
//!
//! A new server takes the next free id here, an initramfs name, and a
//! function under `service` that maps its pages and installs capabilities
//! before `sched::spawn_user_elf`. Install order is the handle order.

/// Boot thread on the bootstrap processor.
pub const IDLE: u8 = 0;
/// Idle thread of CPU 1. `prepare_ap_idle` uses the CPU index as the task id.
pub const AP_IDLE: u8 = 1;
/// Capability mint target. Not a running server.
pub const MINT_TARGET: u8 = 4;
/// Kernel task affine to CPU 1, spawned on CPU 0 so the steal path runs.
pub const AP_PROOF: u8 = 8;
/// Ring-3 stub that lives in the kernel page tables.
pub const USER_STUB: u8 = 9;
pub const VFS: u8 = 10;
pub const BLK: u8 = 11;
pub const COMPOSITOR: u8 = 12;
/// Terminal client. The desktop still calls this the client program.
pub const TERMINAL: u8 = 13;
pub const INPUT: u8 = 14;
pub const FILES: u8 = 15;
/// Calculator. Id 2 is free only while the machine has two CPUs.
pub const CALC: u8 = 2;
pub const MAX: usize = 16;

const _: () = assert!(AP_IDLE == 1);
const _: () = assert!(MAX == 16);
