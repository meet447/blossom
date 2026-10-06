//! Spawned programs: dynamic ids, exit, and parent wait.

extern crate alloc;

use alloc::vec;
use crate::cap;
use crate::exec;
use crate::ipc;
use crate::mm::{self, UserPerm};
use crate::sched;
use crate::task::{self, DYN_FIRST, DYN_LAST};
use core::sync::atomic::{AtomicU64, Ordering};
use meuxe_abi::{ERR_FAULT, ERR_INVAL, ERR_NOMEM, USER_IMAGE, USER_SHARE};
use meuxe_cap::{ObjectKind, TaskId};

const IMAGE_BYTES: u64 = 256 * 1024;
const HELLO: &[u8] = b"hello from disk";

static DYN_USED: AtomicU64 = AtomicU64::new((1u64 << DYN_FIRST as u64) - 1);

static mut CHILD_PARENT: [u8; task::MAX] = [0; task::MAX];
static mut CHILD_EXIT: [i32; task::MAX] = [0; task::MAX];
static mut CHILD_ZOMBIE: [bool; task::MAX] = [false; task::MAX];
static mut PARENT_WAITING: [bool; task::MAX] = [false; task::MAX];
static SHARE_PHYS: AtomicU64 = AtomicU64::new(0);
static mut CHILD_CR3: [u64; task::MAX] = [0; task::MAX];

pub fn init_terminal(share_frame: u64) {
    SHARE_PHYS.store(share_frame, Ordering::Release);
}

fn share_phys() -> u64 {
    SHARE_PHYS.load(Ordering::Acquire)
}

pub fn alloc_id() -> Option<u8> {
    let used = DYN_USED.load(Ordering::Acquire);
    for id in DYN_FIRST..=DYN_LAST {
        let bit = 1u64 << id;
        if used & bit == 0 {
            if DYN_USED
                .compare_exchange(used, used | bit, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(id);
            }
            return alloc_id();
        }
    }
    None
}

fn free_id(id: u8) {
    DYN_USED.fetch_and(!(1u64 << id), Ordering::AcqRel);
}

pub fn spawn(parent: u8, image_addr: u64, image_len: u64) -> u64 {
    if image_len == 0 || image_len > IMAGE_BYTES {
        return ERR_INVAL as u64;
    }
    if image_addr < USER_IMAGE || image_addr + image_len > USER_IMAGE + IMAGE_BYTES {
        return ERR_INVAL as u64;
    }
    if parent_child(parent) != 0 {
        return ERR_INVAL as u64;
    }
    let Some(child) = alloc_id() else {
        return ERR_NOMEM as u64;
    };
    let len = image_len as usize;
    let mut buf = vec![0u8; len];
    if !copy_from_task(parent, image_addr, &mut buf) {
        free_id(child);
        return ERR_FAULT as u64;
    }
    let loaded = match exec::load(&buf) {
        Ok(elf) => elf,
        Err(_) => {
            free_id(child);
            return ERR_INVAL as u64;
        }
    };
    unsafe {
        CHILD_CR3[child as usize] = loaded.cr3;
    }
    if let Err(_) = map_child_share(loaded.cr3) {
        free_id(child);
        return ERR_NOMEM as u64;
    }
    if let Err(_) = install_caps(parent, child, loaded.ring_phys) {
        free_id(child);
        return ERR_NOMEM as u64;
    }
    crate::kprintln!("meuxe: spawn task={child}");
    sched::spawn_user_elf(child, loaded.entry, loaded.cr3);
    unsafe {
        CHILD_PARENT[child as usize] = parent;
        CHILD_ZOMBIE[child as usize] = false;
        CHILD_EXIT[child as usize] = 0;
        PARENT_WAITING[parent as usize] = true;
    }
    wait_child(parent, child);
    let code = unsafe { CHILD_EXIT[child as usize] };
    reap(child);
    if code == ERR_FAULT {
        ERR_FAULT as u64
    } else {
        code as u64
    }
}

pub fn exit(task: u8, code: u32) -> ! {
    if task < DYN_FIRST {
        crate::log::fault(0, 0, 0, 0);
        crate::arch::x86_64::cpu::halt_forever();
    }
    note_hello(task);
    unsafe {
        CHILD_EXIT[task as usize] = code as i32;
        CHILD_ZOMBIE[task as usize] = true;
    }
    crate::kprintln!("meuxe: exit task={task} code={code}");
    let parent = unsafe { CHILD_PARENT[task as usize] };
    if parent != 0 {
        unsafe {
            PARENT_WAITING[parent as usize] = false;
        }
    }
    sched::park_current();
}

pub fn kill_fault(task: u8, vector: u64, cr2: u64) {
    if unsafe { CHILD_ZOMBIE[task as usize] } {
        return;
    }
    crate::kprintln!(
        "meuxe: fault task={task} vector={vector} cr2={cr2:#x} killed"
    );
    unsafe {
        CHILD_EXIT[task as usize] = ERR_FAULT;
        CHILD_ZOMBIE[task as usize] = true;
    }
    let parent = unsafe { CHILD_PARENT[task as usize] };
    if parent != 0 {
        unsafe {
            PARENT_WAITING[parent as usize] = false;
        }
    }
    sched::retire(task);
}

fn wait_child(parent: u8, child: u8) {
    let cpu = crate::arch::x86_64::percpu::this();
    while !unsafe { CHILD_ZOMBIE[child as usize] } {
        unsafe {
            (*cpu).yield_requested.store(1, Ordering::Release);
        }
        crate::arch::x86_64::cpu::sti();
        crate::arch::x86_64::cpu::hlt();
        crate::arch::x86_64::cpu::cli();
    }
    unsafe {
        PARENT_WAITING[parent as usize] = false;
    }
}

fn reap(child: u8) {
    sched::retire(child);
    cap::with_mut(|caps| {
        let child_task = TaskId::new(child).unwrap();
        caps.revoke_task(child_task);
    });
    unsafe {
        CHILD_PARENT[child as usize] = 0;
        CHILD_ZOMBIE[child as usize] = false;
        CHILD_CR3[child as usize] = 0;
    }
}

fn parent_child(parent: u8) -> u8 {
    for id in DYN_FIRST..=DYN_LAST {
        if unsafe { CHILD_PARENT[id as usize] == parent && !CHILD_ZOMBIE[id as usize] } {
            return id;
        }
    }
    0
}

fn note_hello(task: u8) {
    let phys = share_phys();
    if phys == 0 {
        return;
    }
    let ptr = (mm::hhdm() + phys) as *const u8;
    let mut ok = true;
    for (index, byte) in HELLO.iter().enumerate() {
        if unsafe { ptr.add(index).read_volatile() } != *byte {
            ok = false;
            break;
        }
    }
    if ok {
        crate::kprintln!("meuxe: child task={task} says hello from disk");
    }
}

fn map_child_share(cr3: u64) -> Result<(), &'static str> {
    let phys = share_phys();
    if phys == 0 {
        return Err("share frame is missing");
    }
    unsafe {
        core::ptr::write_bytes((mm::hhdm() + phys) as *mut u8, 0, 4096);
    }
    mm::map_user_in(cr3, USER_SHARE, phys, UserPerm::Rw)?;
    Ok(())
}

fn install_caps(parent: u8, child: u8, ring_phys: u64) -> Result<(), &'static str> {
    let parent_task = TaskId::new(parent).ok_or("parent id")?;
    let child_task = TaskId::new(child).ok_or("child id")?;
    cap::with_mut(|caps| {
        let channel = caps
            .create(ObjectKind::Endpoint { parked: None })
            .map_err(|_| "endpoint full")?;
        let ring = caps
            .create(ObjectKind::Frame { phys: ring_phys })
            .map_err(|_| "frame full")?;
        let share = caps
            .create(ObjectKind::Frame { phys: share_phys() })
            .map_err(|_| "frame full")?;
        let cnode = caps
            .create(ObjectKind::CNode { task: child_task })
            .map_err(|_| "cnode full")?;
        let rw = meuxe_abi::Rights::READ.union(meuxe_abi::Rights::WRITE);
        caps.install(child_task, channel, rw.union(meuxe_abi::Rights::GRANT))
            .map_err(|_| "install child ep")?;
        caps.install(parent_task, channel, rw)
            .map_err(|_| "install parent ep")?;
        caps.install(child_task, ring, rw)
            .map_err(|_| "install ring")?;
        caps.install(child_task, share, rw)
            .map_err(|_| "install share")?;
        caps.install(parent_task, cnode, meuxe_abi::Rights::READ)
            .map_err(|_| "install cnode")?;
        ipc::register_ring(child, ring_phys);
        Ok(())
    })
}

fn copy_from_task(task: u8, addr: u64, out: &mut [u8]) -> bool {
    let cr3 = sched::user_cr3(task);
    let mut offset = 0usize;
    while offset < out.len() {
        let virt = addr + offset as u64;
        let page = virt & !0xFFF;
        let page_off = (virt & 0xFFF) as usize;
        let Some(leaf) = mm::leaf_user(cr3, page) else {
            return false;
        };
        if leaf & 2 == 0 {
            return false;
        }
        let phys = leaf & 0x000F_FFFF_FFFF_F000;
        let src = (mm::hhdm() + phys + page_off as u64) as *const u8;
        let chunk = (4096 - page_off).min(out.len() - offset);
        for index in 0..chunk {
            out[offset + index] = unsafe { src.add(index).read_volatile() };
        }
        offset += chunk;
    }
    true
}
