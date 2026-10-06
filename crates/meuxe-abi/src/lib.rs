//! Userspace-visible Meuxe contracts.
//!
//! These types are the capability boundary: unforgeable handles are slot indices,
//! and asynchronous IPC is a single-producer single-consumer ring.

#![no_std]

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

/// Rights a capability may carry. Ambient uid/gid does not exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rights(u8);

impl Rights {
    pub const EMPTY: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const GRANT: Self = Self(1 << 3);

    pub const fn empty() -> Self {
        Self::EMPTY
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

/// Slot index inside a task's capability space. Zero is the null capability.
/// Userspace cannot forge a different object by changing bits in this value;
/// the kernel is the only writer of the slot table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct CapHandle(u32);

impl CapHandle {
    pub const NULL: Self = Self(0);

    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CapType {
    Null = 0,
    Endpoint = 1,
    Frame = 2,
    Irq = 3,
    CNode = 4,
    IoPort = 5,
    Mmio = 6,
}

pub const SQ_OPCODE_NOP: u16 = 0;
pub const SQ_OPCODE_SEND: u16 = 1;
pub const SQ_OPCODE_RECV: u16 = 2;
pub const SQ_OPCODE_MAP: u16 = 3;
pub const SQ_OPCODE_WAIT: u16 = 4;

/// One submission-queue entry. Userspace writes these; the kernel consumes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct SubmissionEntry {
    pub opcode: u16,
    pub flags: u16,
    pub cap: u32,
    pub a: u64,
    pub b: u64,
    pub user_data: u64,
}

/// One completion-queue entry. The kernel publishes these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CompletionEntry {
    pub user_data: u64,
    pub result: i32,
    pub flags: u32,
}

const _: () = assert!(core::mem::size_of::<SubmissionEntry>() == 32);
const _: () = assert!(core::mem::align_of::<SubmissionEntry>() == 8);
const _: () = assert!(core::mem::size_of::<CompletionEntry>() == 16);
const _: () = assert!(core::mem::align_of::<CompletionEntry>() == 8);

pub const RESULT_OK: i32 = 0;
pub const ERR_PERM: i32 = -1;
pub const ERR_BADF: i32 = -9;
pub const ERR_AGAIN: i32 = -11;
pub const ERR_NOMEM: i32 = -12;
pub const ERR_FAULT: i32 = -14;
pub const ERR_INVAL: i32 = -22;

/// MAP completion requests a writable view of the frame. The page-frame number
/// comes back in [`CompletionEntry::flags`].
pub const SQ_FLAG_MAP_WRITE: u16 = 1;

pub const SYS_TASK_ID: u64 = 0;
pub const SYS_RING_PROCESS: u64 = 1;
pub const SYS_YIELD: u64 = 2;
/// Report a slice of the caller's address space. `a0` is the pointer, `a1` the length.
pub const SYS_REPORT: u64 = 3;
/// Sleep until the virtio-blk completion interrupt arrives.
pub const SYS_WAIT_IRQ: u64 = 4;
/// Load an ELF image from the caller's address space and run it.
pub const SYS_SPAWN: u64 = 5;
/// Terminate the current spawned task.
pub const SYS_EXIT: u64 = 6;

pub const USER_CHILD: u64 = 0xE40000;
/// Child view of the share frame the parent reads at `USER_CHILD`.
pub const USER_CHILD_SHARE: u64 = USER_SHARE;
pub const USER_IMAGE: u64 = 0x14000000;
pub const USER_IMAGE_BYTES: u64 = 256 * 1024;

pub const USER_INFO: u64 = 0xB00000;
pub const USER_MMIO: u64 = 0xC00000;
pub const USER_QUEUE: u64 = 0xE00000;
pub const USER_SHARE: u64 = 0xE20000;
pub const USER_FRONT: u64 = 0xF00000;
pub const USER_BACK: u64 = 0xF01000;
pub const USER_STATUS: u64 = 0xF02000;
pub const USER_FB: u64 = 0x10000000;
pub const USER_TERM: u64 = 0x11000000;
pub const USER_KBD_QUEUE: u64 = 0xE10000;
pub const USER_KBD_EVENT: u64 = 0xE30000;
pub const USER_KBD_READY: u64 = 0xF03000;
/// Shared page for the shell's directory requests and the VFS replies.
pub const USER_FS: u64 = 0xF04000;
/// Second directory page, mapped into the VFS beside `USER_FS`.
pub const USER_FS_FILES: u64 = 0xF06000;
/// Pointer clicks inside the file manager, shared with the compositor.
pub const USER_PICK: u64 = 0xF05000;
pub const USER_FILES: u64 = 0x12000000;
/// Calculator surface, shared by the compositor and `meuxe-calc`.
pub const USER_CALC: u64 = 0x13000000;
/// Pointer clicks inside the calculator. Separate from the file manager's pick page.
pub const USER_CALC_PICK: u64 = 0xF07000;
/// 64 KiB DMA window for the virtio-net driver (below `USER_FRONT`).
pub const USER_NET_DMA: u64 = 0xE50000;
/// Terminal ↔ net server RPC page.
pub const USER_NET: u64 = 0xF08000;
pub const USER_KBD_INFO: u64 = USER_INFO + 256;

pub const NET_OP_INFO: u32 = 0;
pub const NET_OP_PING: u32 = 1;
pub const NET_OP_GET: u32 = 2;

pub const NET_OFF_OP: u64 = 0;
pub const NET_OFF_IP: u64 = 4;
pub const NET_OFF_PATH: u64 = 8;
pub const NET_OFF_PATH_LEN: u64 = 72;
pub const NET_OFF_STATUS: u64 = 76;
pub const NET_OFF_REPLY_LEN: u64 = 80;
pub const NET_OFF_REPLY: u64 = 84;

pub const WINDOW_X: u32 = 120;
pub const WINDOW_Y: u32 = 120;
pub const WINDOW_W: u32 = 16;
pub const WINDOW_H: u32 = 16;
pub const WINDOW_COLOR: u32 = 0x00E0_7A3D;

pub const fn pack_rect(x: u32, y: u32, w: u32, h: u32) -> u64 {
    (x as u64) | ((y as u64) << 16) | ((w as u64) << 32) | ((h as u64) << 48)
}

/// Set on the x coordinate while the tablet button is held.
pub const POINTER_HELD: u32 = 1 << 31;

pub const fn pack_pointer(x: u32, y: u32) -> u64 {
    (x as u64) | ((y as u64) << 32)
}

pub const fn pack_pointer_button(x: u32, y: u32, held: bool) -> u64 {
    let x = if held { x | POINTER_HELD } else { x & !POINTER_HELD };
    pack_pointer(x, y)
}

/// A key-down from the virtio keyboard. Bit 63 keeps it distinct from a pointer.
pub const fn pack_key(code: u16) -> u64 {
    (1u64 << 63) | (code as u64)
}

pub const TERM_X: u32 = 64;
pub const TERM_Y: u32 = 200;
pub const TERM_COLS: u32 = 48;
pub const TERM_ROWS: u32 = 16;
pub const TERM_SCALE: u32 = 2;
pub const TERM_FG: u32 = 0x00E7_E1D5;
pub const TERM_BG: u32 = 0x001A_1C24;
pub const TERM_ACCENT: u32 = 0x00E0_7A3D;
pub const TERM_PX_W: u32 = TERM_COLS * 8 * TERM_SCALE;
pub const TERM_PX_H: u32 = TERM_ROWS * 8 * TERM_SCALE;

/// Chrome around a window. The terminal cells stay at `TERM_X`/`TERM_Y`.
pub const TITLE_H: u32 = 22;
pub const FRAME_BORDER: u32 = 2;

pub const FILES_X: u32 = 856;
pub const FILES_Y: u32 = 200;
pub const FILES_COLS: u32 = 26;
pub const FILES_ROWS: u32 = 16;
pub const FILES_SCALE: u32 = 2;
pub const FILES_PX_W: u32 = FILES_COLS * 8 * FILES_SCALE;
pub const FILES_PX_H: u32 = FILES_ROWS * 8 * FILES_SCALE;

/// Calculator content. It sits under the terminal, clear of the 16×16 window
/// and of the cells `make verify` samples.
pub const CALC_X: u32 = 64;
pub const CALC_Y: u32 = 492;
pub const CALC_COLS: u32 = 22;
pub const CALC_ROWS: u32 = 12;
pub const CALC_SCALE: u32 = 2;
pub const CALC_PX_W: u32 = CALC_COLS * 8 * CALC_SCALE;
pub const CALC_PX_H: u32 = CALC_ROWS * 8 * CALC_SCALE;

/// What the kernel tells the block driver about a virtio-blk device.
///
/// The three bases are the starts of the structures, already translated into
/// the driver's address space. `notify` is the base of the notify region;
/// the driver adds `queue_notify_off * notify_mul`.
#[repr(C)]
pub struct BlkBoot {
    pub common: u64,
    pub notify: u64,
    pub device: u64,
    pub notify_mul: u32,
    pub _pad: u32,
    pub queue_phys: u64,
    pub share_phys: u64,
    pub queue_virt: u64,
    pub share_virt: u64,
    /// Physical addresses of the eight data pages at `share_virt + 4096`.
    pub data_phys: [u64; 8],
}

const _: () = assert!(core::mem::size_of::<BlkBoot>() == 128);

/// Virtio-net windows and DMA layout for the network server.
#[repr(C)]
pub struct NetBoot {
    pub common: u64,
    pub notify: u64,
    pub device: u64,
    pub notify_mul: u32,
    pub queue_size: u32,
    pub dma_phys: [u64; 16],
    pub dma_virt: u64,
    pub share_phys: u64,
    pub share_virt: u64,
    pub tick_phys: u64,
    pub tick_virt: u64,
    pub isr: u64,
}

const _: () = assert!(core::mem::size_of::<NetBoot>() == 208);

/// Framebuffer and the two client buffers, in the compositor's address space.
#[repr(C)]
pub struct CompositorBoot {
    pub fb: u64,
    pub front: u64,
    pub back: u64,
    pub status: u64,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
    pub _pad: u8,
    pub term: u64,
    pub term_stride: u32,
    pub term_x: u32,
    pub term_y: u32,
    pub _term_pad: u32,
    pub files: u64,
    pub files_stride: u32,
    pub files_x: u32,
    pub files_y: u32,
    pub _files_pad: u32,
    pub pick: u64,
    pub calc: u64,
    pub calc_stride: u32,
    pub calc_x: u32,
    pub calc_y: u32,
    pub _calc_pad: u32,
    pub calc_pick: u64,
}

const _: () = assert!(core::mem::size_of::<CompositorBoot>() == 136);

/// Virtio-tablet windows plus the screen size used to scale absolute axes.
#[repr(C)]
pub struct InputBoot {
    pub common: u64,
    pub notify: u64,
    pub notify_mul: u32,
    pub width: u32,
    pub height: u32,
    pub _pad: u32,
    pub queue_phys: u64,
    pub event_phys: u64,
    pub queue_virt: u64,
    pub event_virt: u64,
    pub ready: u64,
}

const _: () = assert!(core::mem::size_of::<InputBoot>() == 72);

/// Virtio-keyboard windows. The ready byte is separate from the tablet's.
#[repr(C)]
pub struct KeyboardBoot {
    pub common: u64,
    pub notify: u64,
    pub notify_mul: u32,
    pub _pad: u32,
    pub queue_phys: u64,
    pub event_phys: u64,
    pub queue_virt: u64,
    pub event_virt: u64,
    pub ready: u64,
}

const _: () = assert!(core::mem::size_of::<KeyboardBoot>() == 64);

/// Where the client writes the back buffer.
#[repr(C)]
pub struct ClientBoot {
    pub back: u64,
    pub width: u32,
    pub height: u32,
    pub origin_x: u32,
    pub origin_y: u32,
    pub term: u64,
    pub term_stride: u32,
    pub term_x: u32,
    pub term_y: u32,
    pub cols: u32,
    pub rows: u32,
    pub scale: u32,
    pub screen_w: u32,
    pub screen_h: u32,
}

const _: () = assert!(core::mem::size_of::<ClientBoot>() == 64);

/// File manager surface. Clicks land in `pick` as a sequence, then x and y.
#[repr(C)]
pub struct FilesBoot {
    pub surface: u64,
    pub pick: u64,
    pub stride: u32,
    pub cols: u32,
    pub rows: u32,
    pub scale: u32,
    pub origin_x: u32,
    pub origin_y: u32,
}

const _: () = assert!(core::mem::size_of::<FilesBoot>() == 40);

/// Calculator surface. Clicks land in `pick` as a sequence, then x and y.
#[repr(C)]
pub struct CalcBoot {
    pub surface: u64,
    pub pick: u64,
    pub stride: u32,
    pub cols: u32,
    pub rows: u32,
    pub scale: u32,
    pub origin_x: u32,
    pub origin_y: u32,
}

const _: () = assert!(core::mem::size_of::<CalcBoot>() == 40);

pub const RING_DEPTH: usize = 16;
pub const RING_SQ_OFFSET: usize = 0;
pub const RING_CQ_OFFSET: usize = 0x400;
pub const RING_MAILBOX_OFFSET: usize = 0x800;

/// In-process single-producer single-consumer ring.
///
/// `N` must be a power of two. The layout is `repr(C)` so a ring-3 stub can
/// publish `tail` at a fixed offset inside a shared page.
#[repr(C)]
pub struct SpscRing<T, const N: usize> {
    slots: [UnsafeCell<MaybeUninit<T>>; N],
    head: AtomicU32,
    tail: AtomicU32,
}

unsafe impl<T: Send, const N: usize> Send for SpscRing<T, N> {}
unsafe impl<T: Send, const N: usize> Sync for SpscRing<T, N> {}

impl<T, const N: usize> SpscRing<T, N> {
    pub const fn new() -> Self {
        assert!(N > 0 && N.is_power_of_two() && N <= u32::MAX as usize);
        Self {
            slots: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
        }
    }

    pub fn capacity(&self) -> u32 {
        N as u32
    }

    pub fn push(&self, value: T) -> Result<(), T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) == N as u32 {
            return Err(value);
        }
        let index = (tail as usize) & (N - 1);
        unsafe {
            (*self.slots[index].get()).write(value);
        }
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    pub fn peek(&self) -> Option<T>
    where
        T: Copy,
    {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let index = (head as usize) & (N - 1);
        Some(unsafe { (*self.slots[index].get()).assume_init_read() })
    }

    pub fn pop(&self) -> Option<T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let index = (head as usize) & (N - 1);
        let value = unsafe { (*self.slots[index].get()).assume_init_read() };
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    pub fn len(&self) -> u32 {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn free(&self) -> u32 {
        self.capacity().wrapping_sub(self.len())
    }
}

/// One shared page: submission queue, completion queue, and an 8-byte mailbox.
#[repr(C, align(4096))]
pub struct RingPage {
    pub sq: SpscRing<SubmissionEntry, 16>,
    _sq_pad: [u8; RING_CQ_OFFSET - SQ_BYTES],
    pub cq: SpscRing<CompletionEntry, 16>,
    _cq_pad: [u8; RING_MAILBOX_OFFSET - (RING_CQ_OFFSET + CQ_BYTES)],
    pub mailbox: [u64; 8],
    _end_pad: [u8; 4096 - (RING_MAILBOX_OFFSET + 64)],
}

const SQ_BYTES: usize = core::mem::size_of::<SpscRing<SubmissionEntry, 16>>();
const CQ_BYTES: usize = core::mem::size_of::<SpscRing<CompletionEntry, 16>>();

const _: () = assert!(SQ_BYTES == 520);
const _: () = assert!(CQ_BYTES == 264);
const _: () = assert!(core::mem::offset_of!(SpscRing<SubmissionEntry, 16>, tail) == 516);
const _: () = assert!(core::mem::offset_of!(SpscRing<CompletionEntry, 16>, tail) == 260);
const _: () = assert!(core::mem::offset_of!(RingPage, cq) == RING_CQ_OFFSET);
const _: () = assert!(core::mem::offset_of!(RingPage, mailbox) == RING_MAILBOX_OFFSET);
const _: () = assert!(core::mem::size_of::<RingPage>() == 4096);

impl RingPage {
    pub const fn new() -> Self {
        Self {
            sq: SpscRing::new(),
            _sq_pad: [0; RING_CQ_OFFSET - SQ_BYTES],
            cq: SpscRing::new(),
            _cq_pad: [0; RING_MAILBOX_OFFSET - (RING_CQ_OFFSET + CQ_BYTES)],
            mailbox: [0; 8],
            _end_pad: [0; 4096 - (RING_MAILBOX_OFFSET + 64)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_button_uses_the_high_bit_of_x() {
        let up = pack_pointer_button(128, 64, false);
        let down = pack_pointer_button(128, 64, true);
        assert_eq!(up, pack_pointer(128, 64));
        assert_eq!(down as u32 & POINTER_HELD, POINTER_HELD);
        assert_eq!(down as u32 & !POINTER_HELD, 128);
        assert_eq!(down >> 32, 64);
    }

    #[test]
    fn rights_compose() {
        let rw = Rights::READ.union(Rights::WRITE);
        assert!(rw.contains(Rights::READ));
        assert!(!rw.contains(Rights::GRANT));
        assert_eq!(rw.intersection(Rights::WRITE), Rights::WRITE);
        assert!(CapHandle::NULL.is_null());
        assert!(!CapHandle::new(1).is_null());
    }

    #[test]
    fn ring_fills_and_drains_in_order() {
        let ring = SpscRing::<u32, 4>::new();
        assert!(ring.is_empty());
        assert!(ring.push(1).is_ok());
        assert!(ring.push(2).is_ok());
        assert!(ring.push(3).is_ok());
        assert!(ring.push(4).is_ok());
        assert_eq!(ring.push(5), Err(5));
        assert_eq!(ring.pop(), Some(1));
        assert!(ring.push(5).is_ok());
        assert_eq!(ring.pop(), Some(2));
        assert_eq!(ring.pop(), Some(3));
        assert_eq!(ring.pop(), Some(4));
        assert_eq!(ring.pop(), Some(5));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn submission_entry_carries_a_handle() {
        let entry = SubmissionEntry {
            opcode: SQ_OPCODE_SEND,
            flags: 0,
            cap: CapHandle::new(4).raw(),
            a: 0,
            b: 0,
            user_data: 9,
        };
        assert_eq!(entry.opcode, SQ_OPCODE_SEND);
        assert_eq!(entry.user_data, 9);
        assert_eq!(CapType::Endpoint as u8, 1);
    }
}
