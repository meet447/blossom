//! Task ids for the alpha kernel.
//!
//! The scheduler table holds [`MAX`] tasks. Ids 0 through 7 are reserved for
//! idle threads, one per CPU index, so a later CPU cannot collide with a
//! server. Alpha still boots two CPUs. Ids 24 through 63 are for programs
//! the shell spawns.
//!
//! A new server takes an id in the static range, an initramfs name, and a
//! function under `service` that maps its pages and installs capabilities
//! before `sched::spawn_user_elf`. Install order is the handle order.

/// Boot thread on the bootstrap processor.
pub const IDLE: u8 = 0;
/// Idle thread of CPU 1. `prepare_ap_idle` uses the CPU index as the task id.
pub const AP_IDLE: u8 = 1;
/// Last id reserved for an idle thread. Ids 0 through this value stay free of servers.
pub const IDLE_LAST: u8 = 7;
/// Capability mint target. Not a running server.
pub const MINT_TARGET: u8 = 18;
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
/// Calculator. Kept out of the idle-id range.
pub const CALC: u8 = 16;
/// Virtio-net driver and the in-tree stack.
pub const NET: u8 = 17;
/// First id `sched` may hand to a spawned program.
pub const DYN_FIRST: u8 = 24;
/// Last id in the table.
pub const DYN_LAST: u8 = 63;
pub const MAX: usize = 64;

const _: () = assert!(AP_IDLE == 1);
const _: () = assert!(IDLE_LAST == 7);
const _: () = assert!(CALC >= 8 && NET >= 8 && MINT_TARGET >= 8);
const _: () = assert!(DYN_FIRST == 24);
const _: () = assert!((DYN_LAST as usize) + 1 == MAX);
