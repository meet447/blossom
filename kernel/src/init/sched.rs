//! Capability space, the second CPU, and the ring-3 stub.

use crate::arch::x86_64::{cpu, smp, syscall};
use crate::boot::BootInfo;
use crate::mm;
use crate::sched;
use crate::task;
use core::sync::atomic::Ordering;

pub fn bringup(boot: &BootInfo) -> Result<(), &'static str> {
    let ring = crate::user::setup()?;
    crate::cap::init(ring)?;
    crate::kprintln!(
        "meuxe: user text={:#x} stack={:#x} ring={:#x}",
        mm::layout::USER_TEXT,
        mm::layout::USER_STACK,
        mm::layout::USER_RING
    );
    let online = smp::start(boot)?;
    crate::kprintln!("meuxe: cpus_online={online}");
    if online < 2 {
        return Err("expected two cpus");
    }
    sched::enable();
    sched::spawn_kernel(task::AP_PROOF, sched::ap_proof, 0, 1 << 1);
    let start = sched::ticks();
    while sched::AP_COUNT.load(Ordering::Acquire) == 0 {
        if sched::ticks().wrapping_sub(start) > 300 {
            return Err("ap task did not run");
        }
        cpu::hlt();
    }
    let ran = sched::ran_on(task::AP_PROOF);
    let steals = sched::steals(1);
    crate::kprintln!(
        "meuxe: sched ap_task={} cpu={ran} count={} steals={steals}",
        task::AP_PROOF,
        sched::AP_COUNT.load(Ordering::Acquire)
    );
    if ran != 1 || steals == 0 {
        return Err("application processor did not steal the kernel task");
    }
    sched::spawn_user();
    let start = sched::ticks();
    loop {
        if let Some((task_id, status)) = syscall::yield_status() {
            if status != 0 {
                return Err("user program reported a bad status");
            }
            if task_id != task::USER_STUB as u64 {
                return Err("user yield came from the wrong task");
            }
            break;
        }
        if sched::ticks().wrapping_sub(start) > 500 {
            return Err("user task did not yield");
        }
        cpu::hlt();
    }
    super::check_permissions();
    Ok(())
}
