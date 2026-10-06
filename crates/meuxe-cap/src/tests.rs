use super::*;
use crate::cspace::CapError;
use crate::ipc::{CompletionSink, UserMem};
use crate::object::{ObjectKind, TaskId};
use meuxe_abi::{
    CapHandle, CapType, CompletionEntry, Rights, SpscRing, SubmissionEntry, ERR_AGAIN, ERR_BADF,
    ERR_FAULT, ERR_INVAL, ERR_PERM, RESULT_OK, SQ_FLAG_MAP_WRITE, SQ_OPCODE_MAP, SQ_OPCODE_NOP,
    SQ_OPCODE_RECV, SQ_OPCODE_SEND, SQ_OPCODE_WAIT,
};
use std::collections::BTreeMap;

fn task(raw: u8) -> TaskId {
    TaskId::new(raw).unwrap()
}

struct Sink {
    queues: BTreeMap<u8, Vec<CompletionEntry>>,
    limit: u32,
}

impl Sink {
    fn new() -> Self {
        Self {
            queues: BTreeMap::new(),
            limit: 32,
        }
    }

    fn of(&self, task: TaskId) -> &[CompletionEntry] {
        self.queues.get(&task.raw()).map(Vec::as_slice).unwrap_or(&[])
    }
}

impl CompletionSink for Sink {
    fn can_push(&self, task: TaskId, count: u32) -> bool {
        let used = self.of(task).len() as u32;
        used.saturating_add(count) <= self.limit
    }

    fn push(&mut self, task: TaskId, entry: CompletionEntry) {
        self.queues.entry(task.raw()).or_default().push(entry);
    }
}

struct Mem {
    base: u64,
    bytes: Vec<u8>,
    fail: bool,
}

impl Mem {
    fn new(base: u64) -> Self {
        Self {
            base,
            bytes: vec![0; 4096],
            fail: false,
        }
    }

    fn read_u64(&self, addr: u64) -> u64 {
        let index = (addr - self.base) as usize;
        u64::from_le_bytes(self.bytes[index..index + 8].try_into().unwrap())
    }
}

impl UserMem for Mem {
    fn write_u64(&mut self, _task: TaskId, addr: u64, value: u64) -> bool {
        if self.fail || addr < self.base || addr + 8 > self.base + self.bytes.len() as u64 {
            return false;
        }
        let index = (addr - self.base) as usize;
        self.bytes[index..index + 8].copy_from_slice(&value.to_le_bytes());
        true
    }
}

fn entry(opcode: u16, cap: u32, flags: u16, a: u64, user_data: u64) -> SubmissionEntry {
    SubmissionEntry {
        opcode,
        flags,
        cap,
        a,
        b: 0,
        user_data,
    }
}

#[test]
fn mint_requires_subset_and_grant() {
    let mut caps = CapSpace::new();
    let endpoint = caps.create(ObjectKind::Endpoint { parked: None }).unwrap();
    let src = caps
        .install(
            task(1),
            endpoint,
            Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
        )
        .unwrap();
    let minted = caps
        .mint(task(1), src, task(2), Rights::READ)
        .unwrap();
    assert_eq!(minted, CapHandle::new(1));
    assert!(caps
        .mint(task(1), src, task(2), Rights::READ.union(Rights::EXECUTE))
        .is_err_and(|err| err == CapError::RightsExceeded));
    let full = Rights::READ.union(Rights::WRITE).union(Rights::GRANT);
    assert!(caps.mint(task(1), src, task(2), full).is_ok());

    let weak = caps
        .create(ObjectKind::Endpoint { parked: None })
        .unwrap();
    let handle = caps
        .install(task(3), weak, Rights::READ.union(Rights::WRITE))
        .unwrap();
    assert_eq!(
        caps.mint(task(3), handle, task(4), Rights::READ),
        Err(CapError::NoGrant)
    );
}

#[test]
fn forged_handle_is_rejected() {
    let mut caps = CapSpace::new();
    let endpoint = caps.create(ObjectKind::Endpoint { parked: None }).unwrap();
    let handle = caps
        .install(task(1), endpoint, Rights::READ.union(Rights::GRANT))
        .unwrap();
    assert_eq!(
        caps.lookup(task(1), CapHandle::NULL, Rights::EMPTY, None).err(),
        Some(CapError::Null)
    );
    assert_eq!(
        caps.lookup(task(1), CapHandle::new(2), Rights::EMPTY, None).err(),
        Some(CapError::BadHandle)
    );
    assert_eq!(
        caps.lookup(task(1), CapHandle::new(u32::MAX), Rights::EMPTY, None)
            .err(),
        Some(CapError::BadHandle)
    );
    assert_eq!(
        caps.lookup(task(2), handle, Rights::READ, None).err(),
        Some(CapError::BadHandle)
    );
}

#[test]
fn lookup_checks_rights_and_type() {
    let mut caps = CapSpace::new();
    let endpoint = caps.create(ObjectKind::Endpoint { parked: None }).unwrap();
    let frame = caps.create(ObjectKind::Frame { phys: 0x2000 }).unwrap();
    let ep = caps.install(task(1), endpoint, Rights::READ).unwrap();
    let fr = caps
        .install(task(1), frame, Rights::READ.union(Rights::WRITE))
        .unwrap();
    assert!(caps.lookup(task(1), ep, Rights::WRITE, None).is_err());
    assert!(caps.lookup(task(1), ep, Rights::READ, None).is_ok());
    assert_eq!(
        caps.lookup(task(1), fr, Rights::READ, Some(CapType::Endpoint)).err(),
        Some(CapError::WrongType)
    );
}

#[test]
fn grant_into_another_task_keeps_source() {
    let mut caps = CapSpace::new();
    let endpoint = caps.create(ObjectKind::Endpoint { parked: None }).unwrap();
    let src = caps
        .install(
            task(1),
            endpoint,
            Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
        )
        .unwrap();
    let dst = caps.mint(task(1), src, task(2), Rights::READ).unwrap();
    assert_eq!(dst.raw(), 1);
    assert!(caps.lookup(task(2), dst, Rights::READ, None).is_ok());
    assert!(caps.lookup(task(2), dst, Rights::WRITE, None).is_err());
    assert_eq!(
        caps.mint(task(2), dst, task(3), Rights::READ),
        Err(CapError::NoGrant)
    );
    assert!(caps
        .lookup(task(1), src, Rights::WRITE, Some(CapType::Endpoint))
        .is_ok());
}

#[test]
fn handles_are_one_based() {
    let mut caps = CapSpace::new();
    let mut handles = [CapHandle::NULL; 3];
    for slot in &mut handles {
        let object = caps.create(ObjectKind::Irq { vector: 32 }).unwrap();
        *slot = caps.install(task(0), object, Rights::READ).unwrap();
    }
    assert_eq!(handles.map(|handle| handle.raw()), [1, 2, 3]);
}

#[test]
fn nop_send_recv_and_map() {
    let mut caps = CapSpace::new();
    let endpoint = caps.create(ObjectKind::Endpoint { parked: None }).unwrap();
    let frame = caps.create(ObjectKind::Frame { phys: 0x8000 }).unwrap();
    let ep = caps
        .install(
            task(3),
            endpoint,
            Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
        )
        .unwrap();
    let fr = caps
        .install(task(3), frame, Rights::READ.union(Rights::WRITE))
        .unwrap();
    let sq = SpscRing::<SubmissionEntry, 16>::new();
    let mailbox = 0x800800u64;
    sq.push(entry(SQ_OPCODE_NOP, 0, 0, 0, 1)).unwrap();
    sq.push(entry(SQ_OPCODE_SEND, ep.raw(), 0, 0x4d45, 2)).unwrap();
    sq.push(entry(SQ_OPCODE_RECV, ep.raw(), 0, mailbox, 3)).unwrap();
    sq.push(entry(SQ_OPCODE_MAP, fr.raw(), SQ_FLAG_MAP_WRITE, 0, 4))
        .unwrap();
    let mut sink = Sink::new();
    let mut mem = Mem::new(0x800000);
    let stats = process(&mut caps, task(3), &sq, &mut sink, &mut mem);
    assert_eq!(stats.submitted, 4);
    assert_eq!(stats.completed, 4);
    let done = sink.of(task(3));
    assert_eq!(done.len(), 4);
    assert!(done.iter().all(|entry| entry.result == RESULT_OK));
    assert_eq!(done[3].flags, 0x8000 >> 12);
    assert_eq!(mem.read_u64(mailbox), 0x4d45);
    assert!(sq.is_empty());
}

#[test]
fn recv_then_send_across_tasks() {
    let mut caps = CapSpace::new();
    let endpoint = caps.create(ObjectKind::Endpoint { parked: None }).unwrap();
    let writer = caps
        .install(
            task(1),
            endpoint,
            Rights::WRITE.union(Rights::READ).union(Rights::GRANT),
        )
        .unwrap();
    let reader = caps.mint(task(1), writer, task(2), Rights::READ).unwrap();
    let sq_a = SpscRing::<SubmissionEntry, 8>::new();
    let sq_b = SpscRing::<SubmissionEntry, 8>::new();
    sq_b.push(entry(SQ_OPCODE_RECV, reader.raw(), 0, 0x1000, 9))
        .unwrap();
    sq_a.push(entry(SQ_OPCODE_SEND, writer.raw(), 0, 7, 4))
        .unwrap();
    let mut sink = Sink::new();
    let mut mem = Mem::new(0x1000);
    let parked = process(&mut caps, task(2), &sq_b, &mut sink, &mut mem);
    assert_eq!(parked.completed, 0);
    let sent = process(&mut caps, task(1), &sq_a, &mut sink, &mut mem);
    assert_eq!(sent.completed, 2);
    assert_eq!(mem.read_u64(0x1000), 7);
    assert_eq!(sink.of(task(2))[0].user_data, 9);
    assert_eq!(sink.of(task(1))[0].user_data, 4);
}

#[test]
fn rights_and_bad_opcodes_complete_with_errors() {
    let mut caps = CapSpace::new();
    let endpoint = caps.create(ObjectKind::Endpoint { parked: None }).unwrap();
    let frame = caps.create(ObjectKind::Frame { phys: 0x3000 }).unwrap();
    let ep = caps.install(task(1), endpoint, Rights::READ).unwrap();
    let fr = caps.install(task(1), frame, Rights::READ).unwrap();
    let sq = SpscRing::<SubmissionEntry, 16>::new();
    sq.push(entry(SQ_OPCODE_SEND, ep.raw(), 0, 1, 1)).unwrap();
    sq.push(entry(SQ_OPCODE_RECV, ep.raw(), 0, 1, 2)).unwrap();
    sq.push(entry(SQ_OPCODE_MAP, fr.raw(), SQ_FLAG_MAP_WRITE, 0, 3))
        .unwrap();
    sq.push(entry(9, 0, 0, 0, 4)).unwrap();
    sq.push(entry(SQ_OPCODE_NOP, 0, 0, 0, 5)).unwrap();
    sq.push(entry(SQ_OPCODE_RECV, CapHandle::NULL.raw(), 0, 8, 6))
        .unwrap();
    let mut sink = Sink::new();
    let mut mem = Mem::new(0);
    process(&mut caps, task(1), &sq, &mut sink, &mut mem);
    let done = sink.of(task(1));
    assert_eq!(done[0].result, ERR_PERM);
    assert_eq!(done[1].result, ERR_INVAL);
    assert_eq!(done[2].result, ERR_PERM);
    assert_eq!(done[3].result, ERR_INVAL);
    assert_eq!(done[4].result, RESULT_OK);
    assert_eq!(done[5].result, ERR_BADF);
}

#[test]
fn third_send_gets_eagain_and_mailbox_fault() {
    let mut caps = CapSpace::new();
    let endpoint = caps.create(ObjectKind::Endpoint { parked: None }).unwrap();
    let handle = caps
        .install(task(1), endpoint, Rights::READ.union(Rights::WRITE))
        .unwrap();
    let sq = SpscRing::<SubmissionEntry, 8>::new();
    sq.push(entry(SQ_OPCODE_SEND, handle.raw(), 0, 1, 1)).unwrap();
    sq.push(entry(SQ_OPCODE_SEND, handle.raw(), 0, 2, 2)).unwrap();
    let mut sink = Sink::new();
    let mut mem = Mem::new(0x2000);
    mem.fail = true;
    let stats = process(&mut caps, task(1), &sq, &mut sink, &mut mem);
    assert_eq!(stats.completed, 1);
    assert_eq!(sink.of(task(1))[0].result, ERR_AGAIN);
    sq.push(entry(SQ_OPCODE_RECV, handle.raw(), 0, 0x2000, 3))
        .unwrap();
    process(&mut caps, task(1), &sq, &mut sink, &mut mem);
    assert!(sink.of(task(1)).iter().any(|entry| entry.result == ERR_FAULT));
}

#[test]
fn full_completion_queue_leaves_the_submission() {
    let mut caps = CapSpace::new();
    let sq = SpscRing::<SubmissionEntry, 4>::new();
    sq.push(entry(SQ_OPCODE_NOP, 0, 0, 0, 1)).unwrap();
    let mut sink = Sink::new();
    sink.limit = 0;
    let mut mem = Mem::new(0);
    let stats = process(&mut caps, task(0), &sq, &mut sink, &mut mem);
    assert_eq!(stats, ProcessStats { submitted: 0, completed: 0 });
    assert_eq!(sq.len(), 1);
}

#[test]
#[test]
fn cnode_wait_returns_child_id() {
    let mut caps = CapSpace::new();
    let child = task(24);
    let cnode = caps
        .create(ObjectKind::CNode { task: child })
        .unwrap();
    let handle = caps
        .install(task(13), cnode, Rights::READ)
        .unwrap();
    let sq = SpscRing::<SubmissionEntry, 4>::new();
    sq.push(entry(SQ_OPCODE_WAIT, handle.raw(), 0, 0, 9))
        .unwrap();
    let mut sink = Sink::new();
    let mut mem = Mem::new(0);
    process(&mut caps, task(13), &sq, &mut sink, &mut mem);
    let done = sink.of(task(13));
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].result, RESULT_OK);
    assert_eq!(done[0].flags, 24);
    assert_eq!(done[0].user_data, 9);
}

#[test]
fn irq_capability_is_visible_in_slot_order() {
    let mut caps = CapSpace::new();
    let blk = task(11);
    let first = caps.create(ObjectKind::Irq { vector: 33 }).unwrap();
    let second = caps.create(ObjectKind::Irq { vector: 36 }).unwrap();
    caps.install(blk, first, Rights::READ).unwrap();
    caps.install(blk, second, Rights::READ).unwrap();
    assert_eq!(caps.first_irq(blk), Some(33));
    assert!(caps.has_irq(blk, 33));
    assert!(caps.has_irq(blk, 36));
    assert!(!caps.has_irq(blk, 34));
    assert_eq!(caps.first_irq(task(12)), None);
}
