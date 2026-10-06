//! Run a task's ring. Completions and mailbox writes go through the direct
//! map, so a rendezvous can finish a task that lives in another address space.

use crate::cap;
use crate::mm::{self, layout::USER_RING};
use meuxe_abi::{CompletionEntry, RingPage, SubmissionEntry, RING_MAILBOX_OFFSET};
use meuxe_cap::{CompletionSink, ProcessStats, TaskId, UserMem};
use core::sync::atomic::{AtomicU64, Ordering};

const _: () = assert!(core::mem::size_of::<SubmissionEntry>() == 32);
const _: () = assert!(core::mem::size_of::<CompletionEntry>() == 16);

static RINGS: [AtomicU64; meuxe_cap::MAX_TASKS] = [const { AtomicU64::new(0) }; meuxe_cap::MAX_TASKS];

pub fn register_ring(task: u8, phys: u64) {
    RINGS[task as usize].store(phys, Ordering::Release);
}

fn ring_page(task: TaskId) -> Option<&'static RingPage> {
    let phys = RINGS[task.index()].load(Ordering::Acquire);
    if phys == 0 {
        None
    } else {
        Some(unsafe { &*((mm::hhdm() + phys) as *const RingPage) })
    }
}

struct RingSink;

impl CompletionSink for RingSink {
    fn can_push(&self, task: TaskId, count: u32) -> bool {
        ring_page(task).is_some_and(|page| page.cq.free() >= count)
    }

    fn push(&mut self, task: TaskId, entry: CompletionEntry) {
        if let Some(page) = ring_page(task) {
            let _ = page.cq.push(entry);
        }
    }
}

struct RingMem;

impl UserMem for RingMem {
    fn write_u64(&mut self, task: TaskId, addr: u64, value: u64) -> bool {
        let phys = RINGS[task.index()].load(Ordering::Acquire);
        let start = USER_RING + RING_MAILBOX_OFFSET as u64;
        let end = USER_RING + 4096;
        if phys == 0 || addr % 8 != 0 || addr < start || addr.saturating_add(8) > end {
            return false;
        }
        let offset = addr - USER_RING;
        unsafe {
            ((mm::hhdm() + phys + offset) as *mut u64).write_volatile(value);
        }
        true
    }
}

pub fn process_ring(task: u64) -> u32 {
    let Some(task) = TaskId::new(task as u8) else {
        return 0;
    };
    let Some(page) = ring_page(task) else {
        return 0;
    };
    let mut sink = RingSink;
    let mut mem = RingMem;
    let ProcessStats { completed, .. } =
        cap::with_mut(|caps| meuxe_cap::process(caps, task, &page.sq, &mut sink, &mut mem));
    completed
}
