//! Drain a submission queue into endpoint rendezvous and completions.

use crate::cspace::{CapError, CapSpace};
use crate::object::{ObjectKind, Parked, TaskId};
use meuxe_abi::{
    CapHandle, CapType, CompletionEntry, Rights, SpscRing, SubmissionEntry, ERR_AGAIN, ERR_FAULT,
    ERR_INVAL, RESULT_OK, SQ_FLAG_MAP_WRITE, SQ_OPCODE_MAP, SQ_OPCODE_NOP, SQ_OPCODE_RECV,
    SQ_OPCODE_SEND,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessStats {
    pub submitted: u32,
    pub completed: u32,
}

pub trait CompletionSink {
    fn can_push(&self, task: TaskId, count: u32) -> bool;
    fn push(&mut self, task: TaskId, entry: CompletionEntry);
}

pub trait UserMem {
    fn write_u64(&mut self, task: TaskId, addr: u64, value: u64) -> bool;
}

pub fn process<const N: usize>(
    caps: &mut CapSpace,
    task: TaskId,
    sq: &SpscRing<SubmissionEntry, N>,
    sink: &mut dyn CompletionSink,
    mem: &mut dyn UserMem,
) -> ProcessStats {
    let mut stats = ProcessStats::default();
    while let Some(entry) = sq.peek() {
        let need = if entry.opcode == SQ_OPCODE_SEND || entry.opcode == SQ_OPCODE_RECV {
            2
        } else {
            1
        };
        if !sink.can_push(task, need) {
            break;
        }
        let entry = sq.pop().unwrap_or(entry);
        stats.submitted += 1;
        stats.completed += dispatch(caps, task, entry, sink, mem);
    }
    stats
}

fn dispatch(
    caps: &mut CapSpace,
    task: TaskId,
    entry: SubmissionEntry,
    sink: &mut dyn CompletionSink,
    mem: &mut dyn UserMem,
) -> u32 {
    match entry.opcode {
        SQ_OPCODE_NOP => {
            finish(sink, task, entry.user_data, RESULT_OK, 0);
            1
        }
        SQ_OPCODE_SEND => rendezvous(caps, task, entry, sink, mem, true),
        SQ_OPCODE_RECV => rendezvous(caps, task, entry, sink, mem, false),
        SQ_OPCODE_MAP => map_frame(caps, task, entry, sink),
        _ => {
            finish(sink, task, entry.user_data, ERR_INVAL, 0);
            1
        }
    }
}

fn rendezvous(
    caps: &mut CapSpace,
    task: TaskId,
    entry: SubmissionEntry,
    sink: &mut dyn CompletionSink,
    mem: &mut dyn UserMem,
    is_send: bool,
) -> u32 {
    if !is_send && entry.a % 8 != 0 {
        finish(sink, task, entry.user_data, ERR_INVAL, 0);
        return 1;
    }
    let required = if is_send { Rights::WRITE } else { Rights::READ };
    let object = match caps.lookup(
        task,
        CapHandle::new(entry.cap),
        required,
        Some(CapType::Endpoint),
    ) {
        Ok((object, _)) => object,
        Err(error) => {
            finish(sink, task, entry.user_data, error.result(), 0);
            return 1;
        }
    };
    let endpoint = match caps.kind_mut(object) {
        ObjectKind::Endpoint { parked } => parked,
        _ => {
            finish(sink, task, entry.user_data, CapError::WrongType.result(), 0);
            return 1;
        }
    };
    match endpoint.take() {
        Some(Parked::Recv {
            task: waiter,
            user_data,
            mailbox,
        }) if is_send => {
            if !sink.can_push(waiter, 1) {
                *endpoint = Some(Parked::Recv {
                    task: waiter,
                    user_data,
                    mailbox,
                });
                finish(sink, task, entry.user_data, ERR_AGAIN, 0);
                return 1;
            }
            complete_pair(
                sink,
                mem,
                waiter,
                user_data,
                mailbox,
                task,
                entry.user_data,
                entry.a,
            )
        }
        Some(Parked::Send {
            task: waiter,
            user_data,
            payload,
        }) if !is_send => {
            if !sink.can_push(waiter, 1) {
                *endpoint = Some(Parked::Send {
                    task: waiter,
                    user_data,
                    payload,
                });
                finish(sink, task, entry.user_data, ERR_AGAIN, 0);
                return 1;
            }
            complete_pair(
                sink,
                mem,
                task,
                entry.user_data,
                entry.a,
                waiter,
                user_data,
                payload,
            )
        }
        Some(other) => {
            *endpoint = Some(other);
            finish(sink, task, entry.user_data, ERR_AGAIN, 0);
            1
        }
        None => {
            *endpoint = Some(if is_send {
                Parked::Send {
                    task,
                    user_data: entry.user_data,
                    payload: entry.a,
                }
            } else {
                Parked::Recv {
                    task,
                    user_data: entry.user_data,
                    mailbox: entry.a,
                }
            });
            0
        }
    }
}

fn complete_pair(
    sink: &mut dyn CompletionSink,
    mem: &mut dyn UserMem,
    recv_task: TaskId,
    recv_user: u64,
    mailbox: u64,
    send_task: TaskId,
    send_user: u64,
    payload: u64,
) -> u32 {
    let result = if mem.write_u64(recv_task, mailbox, payload) {
        RESULT_OK
    } else {
        ERR_FAULT
    };
    finish(sink, recv_task, recv_user, result, 0);
    finish(sink, send_task, send_user, result, 0);
    2
}

fn map_frame(
    caps: &mut CapSpace,
    task: TaskId,
    entry: SubmissionEntry,
    sink: &mut dyn CompletionSink,
) -> u32 {
    let required = if entry.flags & SQ_FLAG_MAP_WRITE != 0 {
        Rights::WRITE
    } else {
        Rights::READ
    };
    match caps.lookup(
        task,
        CapHandle::new(entry.cap),
        required,
        Some(CapType::Frame),
    ) {
        Ok((_, ObjectKind::Frame { phys })) => {
            finish(sink, task, entry.user_data, RESULT_OK, (phys >> 12) as u32);
        }
        Ok(_) => finish(sink, task, entry.user_data, CapError::WrongType.result(), 0),
        Err(error) => finish(sink, task, entry.user_data, error.result(), 0),
    }
    1
}

fn finish(sink: &mut dyn CompletionSink, task: TaskId, user_data: u64, result: i32, flags: u32) {
    sink.push(
        task,
        CompletionEntry {
            user_data,
            result,
            flags,
        },
    );
}
