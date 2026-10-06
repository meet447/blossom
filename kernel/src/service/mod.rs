//! Userspace servers, started after the scheduler proof.
//!
//! `start` runs storage, then the desktop. Each server is one id from
//! [`crate::task`], one initramfs ELF, and a function that maps pages and
//! installs capabilities before `sched::spawn_user_elf`.
//!
//! Handles are 1-based slot indexes. The order of `install` on a task is the
//! handle order. Append a new capability after the ones a server already
//! names by number.

mod desktop;
pub(crate) mod storage;
pub(crate) mod net;

use crate::boot::BootInfo;

pub fn start(boot: &BootInfo) -> Result<(), &'static str> {
    storage::start()?;
    crate::kprintln!("meuxe: storage ready");
    desktop::start(boot)?;
    crate::kprintln!("meuxe: desktop ready");
    net::start()?;
    crate::kprintln!("meuxe: net ready");
    Ok(())
}
