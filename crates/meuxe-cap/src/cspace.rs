//! Per-task capability slots.

use crate::object::{ObjectId, ObjectKind, ObjectTable, TaskId};
use meuxe_abi::{CapHandle, CapType, Rights};

pub const SLOTS_PER_TASK: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    Null,
    BadHandle,
    Permission,
    NoGrant,
    RightsExceeded,
    WrongType,
    Full,
    BadTask,
}

impl CapError {
    pub const fn result(self) -> i32 {
        match self {
            Self::Null | Self::BadHandle => meuxe_abi::ERR_BADF,
            Self::Permission | Self::NoGrant | Self::RightsExceeded => meuxe_abi::ERR_PERM,
            Self::WrongType => meuxe_abi::ERR_INVAL,
            Self::Full => meuxe_abi::ERR_NOMEM,
            Self::BadTask => meuxe_abi::ERR_INVAL,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub object: u16,
    pub cap_type: CapType,
    pub rights: Rights,
}

impl Slot {
    const fn empty() -> Self {
        Self {
            object: 0,
            cap_type: CapType::Null,
            rights: Rights::EMPTY,
        }
    }

    fn is_empty(self) -> bool {
        matches!(self.cap_type, CapType::Null)
    }
}

struct SlotTable {
    slots: [Slot; SLOTS_PER_TASK],
}

impl SlotTable {
    const fn new() -> Self {
        Self {
            slots: [Slot::empty(); SLOTS_PER_TASK],
        }
    }
}

pub struct CapSpace {
    objects: ObjectTable,
    tasks: [SlotTable; crate::MAX_TASKS],
}

impl CapSpace {
    pub const fn new() -> Self {
        Self {
            objects: ObjectTable::new(),
            tasks: [const { SlotTable::new() }; crate::MAX_TASKS],
        }
    }

    pub fn create(&mut self, kind: ObjectKind) -> Result<ObjectId, CapError> {
        self.objects.create(kind).map_err(|_| CapError::Full)
    }

    pub fn install(
        &mut self,
        task: TaskId,
        object: ObjectId,
        rights: Rights,
    ) -> Result<CapHandle, CapError> {
        let kind = *self.objects.get(object);
        if matches!(kind, ObjectKind::Null) {
            return Err(CapError::BadHandle);
        }
        let table = &mut self.tasks[task.index()];
        for (index, slot) in table.slots.iter_mut().enumerate() {
            if slot.is_empty() {
                *slot = Slot {
                    object: object.raw(),
                    cap_type: kind.cap_type(),
                    rights,
                };
                return Ok(CapHandle::new((index + 1) as u32));
            }
        }
        Err(CapError::Full)
    }

    pub fn lookup(
        &self,
        task: TaskId,
        handle: CapHandle,
        required: Rights,
        expect: Option<CapType>,
    ) -> Result<(ObjectId, ObjectKind), CapError> {
        if handle.is_null() {
            return Err(CapError::Null);
        }
        let index = handle.raw() as usize;
        if index == 0 || index > SLOTS_PER_TASK {
            return Err(CapError::BadHandle);
        }
        let slot = self.tasks[task.index()].slots[index - 1];
        if slot.is_empty() {
            return Err(CapError::BadHandle);
        }
        if let Some(expect) = expect {
            if slot.cap_type as u8 != expect as u8 {
                return Err(CapError::WrongType);
            }
        }
        if !slot.rights.contains(required) {
            return Err(CapError::Permission);
        }
        let Some(object) = ObjectId::from_index(slot.object as usize) else {
            return Err(CapError::BadHandle);
        };
        Ok((object, *self.objects.get(object)))
    }

    pub fn mint(
        &mut self,
        src_task: TaskId,
        src_handle: CapHandle,
        dst_task: TaskId,
        new_rights: Rights,
    ) -> Result<CapHandle, CapError> {
        let (object, kind) = self.lookup(src_task, src_handle, Rights::EMPTY, None)?;
        let slot = self.slot(src_task, src_handle)?;
        if !slot.rights.contains(Rights::GRANT) {
            return Err(CapError::NoGrant);
        }
        if !slot.rights.contains(new_rights) {
            return Err(CapError::RightsExceeded);
        }
        let _ = kind;
        self.install(dst_task, object, new_rights)
    }

    pub fn kind_mut(&mut self, object: ObjectId) -> &mut ObjectKind {
        self.objects.get_mut(object)
    }

    /// Vector of the first `Irq` capability installed in this task, in slot order.
    pub fn first_irq(&self, task: TaskId) -> Option<u32> {
        for slot in &self.tasks[task.index()].slots {
            if slot.cap_type as u8 != CapType::Irq as u8 {
                continue;
            }
            let Some(object) = ObjectId::from_index(slot.object as usize) else {
                continue;
            };
            if let ObjectKind::Irq { vector } = self.objects.get(object) {
                return Some(*vector);
            }
        }
        None
    }

    /// Whether this task holds an `Irq` capability for `vector`.
    pub fn has_irq(&self, task: TaskId, vector: u32) -> bool {
        for slot in &self.tasks[task.index()].slots {
            if slot.cap_type as u8 != CapType::Irq as u8 {
                continue;
            }
            let Some(object) = ObjectId::from_index(slot.object as usize) else {
                continue;
            };
            if let ObjectKind::Irq { vector: have } = self.objects.get(object) {
                if *have == vector {
                    return true;
                }
            }
        }
        false
    }

    fn slot(&self, task: TaskId, handle: CapHandle) -> Result<Slot, CapError> {
        if handle.is_null() {
            return Err(CapError::Null);
        }
        let index = handle.raw() as usize;
        if index == 0 || index > SLOTS_PER_TASK {
            return Err(CapError::BadHandle);
        }
        let slot = self.tasks[task.index()].slots[index - 1];
        if slot.is_empty() {
            Err(CapError::BadHandle)
        } else {
            Ok(slot)
        }
    }
}
