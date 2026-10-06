//! Capability slots and endpoint rendezvous.
//!
//! Handles are 1-based indexes into a task's slot table. The kernel is the
//! only writer. A handle value from another task names nothing here.

#![cfg_attr(not(test), no_std)]

mod cspace;
mod ipc;
mod object;

pub use cspace::{CapError, CapSpace, Slot};
pub use ipc::{process, CompletionSink, ProcessStats, UserMem};
pub use object::{ObjectId, ObjectKind, Parked, TaskId};

pub const MAX_OBJECTS: usize = 64;
pub const MAX_TASKS: usize = 16;
pub const SLOTS_PER_TASK: usize = 32;

#[cfg(test)]
mod tests;
