//! Kernel objects. Slot tables point at these; they are not handles.

use meuxe_abi::CapType;

pub const MAX_OBJECTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectId(u16);

impl ObjectId {
    pub const fn raw(self) -> u16 {
        self.0
    }

    pub const fn from_index(index: usize) -> Option<Self> {
        if index == 0 || index >= MAX_OBJECTS {
            None
        } else {
            Some(Self(index as u16))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskId(u8);

impl TaskId {
    pub const fn new(raw: u8) -> Option<Self> {
        if (raw as usize) < crate::MAX_TASKS {
            Some(Self(raw))
        } else {
            None
        }
    }

    pub const fn raw(self) -> u8 {
        self.0
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parked {
    Send {
        task: TaskId,
        user_data: u64,
        payload: u64,
    },
    Recv {
        task: TaskId,
        user_data: u64,
        mailbox: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Null,
    Endpoint { parked: Option<Parked> },
    Frame { phys: u64 },
    Irq { vector: u32 },
    CNode { task: TaskId },
    IoPort { base: u16, len: u16 },
    Mmio { phys: u64, len: u64 },
}

impl ObjectKind {
    pub const fn cap_type(self) -> CapType {
        match self {
            Self::Null => CapType::Null,
            Self::Endpoint { .. } => CapType::Endpoint,
            Self::Frame { .. } => CapType::Frame,
            Self::Irq { .. } => CapType::Irq,
            Self::CNode { .. } => CapType::CNode,
            Self::IoPort { .. } => CapType::IoPort,
            Self::Mmio { .. } => CapType::Mmio,
        }
    }
}

pub struct ObjectTable {
    slots: [ObjectKind; MAX_OBJECTS],
}

impl ObjectTable {
    pub const fn new() -> Self {
        Self {
            slots: [ObjectKind::Null; MAX_OBJECTS],
        }
    }

    pub fn create(&mut self, kind: ObjectKind) -> Result<ObjectId, ()> {
        if matches!(kind, ObjectKind::Null) {
            return Err(());
        }
        for index in 1..MAX_OBJECTS {
            if matches!(self.slots[index], ObjectKind::Null) {
                self.slots[index] = kind;
                return Ok(ObjectId(index as u16));
            }
        }
        Err(())
    }

    pub fn get(&self, id: ObjectId) -> &ObjectKind {
        &self.slots[id.0 as usize]
    }

    pub fn get_mut(&mut self, id: ObjectId) -> &mut ObjectKind {
        &mut self.slots[id.0 as usize]
    }
}
