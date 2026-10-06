//! Load the VFS and the virtio-blk driver, each in its own address space.
//! The driver reads sector 0 of the raw log. The VFS looks up `note` in it.

use crate::arch::x86_64::cpu;
use crate::cap;
use crate::ipc;
use crate::exec;
use crate::mm::{self, UserPerm};
use crate::dev::pci;
use crate::sched;
use crate::task;
use crate::sync::Mutex;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use meuxe_abi::{BlkBoot, Rights, USER_FS, USER_INFO, USER_MMIO, USER_QUEUE, USER_SHARE};
use meuxe_cap::{ObjectKind, TaskId};
use meuxe_fs::Archive;

const INITRAMFS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/initramfs.bin"));

static BLK_OK: AtomicBool = AtomicBool::new(false);
static VFS_OK: AtomicBool = AtomicBool::new(false);
static DIRECTORY_STORED: AtomicBool = AtomicBool::new(false);
static REPORTS: Mutex<()> = Mutex::new(());
static FS_FRAME: AtomicU64 = AtomicU64::new(0);

pub fn fs_frame() -> u64 {
    FS_FRAME.load(Ordering::Acquire)
}

static VFS_CR3: AtomicU64 = AtomicU64::new(0);

pub fn vfs_cr3() -> u64 {
    VFS_CR3.load(Ordering::Acquire)
}

pub fn start() -> Result<(), &'static str> {
    let archive = Archive::parse(INITRAMFS).map_err(meuxe_fs::FsError::as_str)?;
    let vfs_image = archive.lookup(b"vfs").ok_or("initramfs is missing vfs")?;
    let blk_image = archive.lookup(b"blk").ok_or("initramfs is missing blk")?;
    let device = pci::find_virtio_blk()?;
    crate::kprintln!(
        "meuxe: virtio-blk mmio={:#x} notify={:#x} isr={:#x}/{}",
        device.common,
        device.notify,
        device.isr,
        device.isr_len
    );

    let vfs = exec::load(vfs_image)?;
    VFS_CR3.store(vfs.cr3, Ordering::Release);
    let blk = exec::load(blk_image)?;
    if vfs.cr3 == mm::kernel_cr3() || blk.cr3 == mm::kernel_cr3() || vfs.cr3 == blk.cr3 {
        return Err("address spaces are not distinct");
    }
    crate::kprintln!(
        "meuxe: elf=vfs entry={:#x} cr3={:#x}",
        vfs.entry,
        vfs.cr3
    );
    crate::kprintln!(
        "meuxe: elf=blk entry={:#x} cr3={:#x}",
        blk.entry,
        blk.cr3
    );
    crate::kprintln!("meuxe: cr3 distinct");

    let share = mm::alloc_frame_zeroed()?;
    let queue = mm::alloc_frame_zeroed()?;
    let info = mm::alloc_frame_zeroed()?;
    let fs = mm::alloc_frame_zeroed()?;
    let mut data_phys = [0u64; 8];
    for (index, slot) in data_phys.iter_mut().enumerate() {
        let frame = mm::alloc_frame_zeroed()?;
        let virt = USER_SHARE + ((index as u64) + 1) * 4096;
        mm::map_user_in(blk.cr3, virt, frame, UserPerm::UncachedRw)?;
        *slot = frame;
    }
    mm::map_user_in(vfs.cr3, USER_SHARE, share, UserPerm::UncachedRw)?;
    mm::map_user_in(blk.cr3, USER_SHARE, share, UserPerm::UncachedRw)?;
    mm::map_user_in(blk.cr3, USER_QUEUE, queue, UserPerm::UncachedRw)?;
    mm::map_user_in(vfs.cr3, USER_FS, fs, UserPerm::Rw)?;
    FS_FRAME.store(fs, Ordering::Release);

    let common = map_window(blk.cr3, USER_MMIO, device.common, device.common_len.max(0x40))?;
    let notify = map_window(
        blk.cr3,
        USER_MMIO + 0x10000,
        device.notify,
        device.notify_len.max(4),
    )?;
    let device_va = if device.device == 0 {
        0
    } else {
        map_window(
            blk.cr3,
            USER_MMIO + 0x20000,
            device.device,
            device.device_len.max(8),
        )?
    };
    let boot = BlkBoot {
        common,
        notify,
        device: device_va,
        notify_mul: device.notify_mul,
        _pad: 0,
        queue_phys: queue,
        share_phys: share,
        queue_virt: USER_QUEUE,
        share_virt: USER_SHARE,
        data_phys,
    };
    unsafe {
        ((mm::hhdm() + info) as *mut BlkBoot).write_volatile(boot);
    }
    mm::map_user_in(blk.cr3, USER_INFO, info, UserPerm::Ro)?;

    install_caps(share, device.common)?;
    ipc::register_ring(task::VFS, vfs.ring_phys);
    ipc::register_ring(task::BLK, blk.ring_phys);
    sched::spawn_user_elf(task::VFS, vfs.entry, vfs.cr3);
    sched::spawn_user_elf(task::BLK, blk.entry, blk.cr3);

    let start = sched::ticks();
    while !BLK_OK.load(Ordering::Acquire) || !VFS_OK.load(Ordering::Acquire) {
        if sched::ticks().wrapping_sub(start) > 1000 {
            return Err("vfs and blk did not finish");
        }
        cpu::hlt();
    }
    Ok(())
}

pub fn note_report(task: u64, ptr: u64, len: u64) {
    let _guard = REPORTS.lock();
    let Some(bytes) = copy_user(ptr, len) else {
        crate::kprintln!("meuxe: report task={task} unmapped");
        return;
    };
    crate::kprintln!("meuxe: report task={task} len={len}");
    let share_lo = USER_SHARE + 16;
    let share_hi = share_lo + 512;
    let in_sector = ptr >= share_lo && ptr.saturating_add(len) <= share_hi;
    let write_lo = USER_SHARE + 2048 + 16;
    let write_hi = write_lo + 512;
    let in_write = ptr >= write_lo && ptr.saturating_add(len) <= write_hi;
    if task == task::BLK as u64 && in_sector && &bytes[..len as usize] == b"MXLG" {
        crate::kprintln!("meuxe: blk sector=MXLG");
        BLK_OK.store(true, Ordering::Release);
    }
    if task == task::BLK as u64 && in_write && &bytes[..len as usize] == b"MXLG" {
        if DIRECTORY_STORED.swap(true, Ordering::AcqRel) {
            crate::kprintln!("meuxe: fs write=ok");
        } else {
            crate::kprintln!("meuxe: blk write=ok");
        }
    }
    if task == task::VFS as u64 && in_sector && &bytes[..len as usize] == b"meuxe-phase3" {
        crate::kprintln!("meuxe: vfs note=meuxe-phase3");
        VFS_OK.store(true, Ordering::Release);
    }
    if task == task::BLK as u64 && &bytes[..len as usize] == b"RNG!" {
        crate::kprintln!("meuxe: blk range lba=8 sectors=64 ok");
    }
    if task == task::BLK as u64
        && (1..len as usize).all(|_| true)
        && bytes[..len as usize].iter().all(|byte| byte.is_ascii_digit())
    {
        if let Ok(text) = core::str::from_utf8(&bytes[..len as usize]) {
            crate::kprintln!("meuxe: blk capacity={text}");
        }
    }
}

fn install_caps(share: u64, mmio_phys: u64) -> Result<(), &'static str> {
    let vfs = TaskId::new(task::VFS).ok_or("vfs task id is out of range")?;
    let blk = TaskId::new(task::BLK).ok_or("blk task id is out of range")?;
    cap::with_mut(|caps| {
        let endpoint = caps
            .create(ObjectKind::Endpoint { parked: None })
            .map_err(|_| "endpoint table is full")?;
        let frame = caps
            .create(ObjectKind::Frame { phys: share })
            .map_err(|_| "frame table is full")?;
        let mmio = caps
            .create(ObjectKind::Mmio {
                phys: mmio_phys,
                len: 0x1000,
            })
            .map_err(|_| "mmio table is full")?;
        let rw = Rights::READ.union(Rights::WRITE);
        let vfs_ep = caps
            .install(vfs, endpoint, rw)
            .map_err(|_| "installing the vfs endpoint failed")?;
        let blk_ep = caps
            .install(blk, endpoint, rw)
            .map_err(|_| "installing the blk endpoint failed")?;
        caps.install(vfs, frame, rw)
            .map_err(|_| "installing the vfs frame failed")?;
        caps.install(blk, frame, rw)
            .map_err(|_| "installing the blk frame failed")?;
        let mmio_handle = caps
            .install(blk, mmio, rw)
            .map_err(|_| "installing the blk mmio capability failed")?;
        let irq = caps
            .create(ObjectKind::Irq { vector: 33 })
            .map_err(|_| "irq object table is full")?;
        caps.install(blk, irq, Rights::READ)
            .map_err(|_| "installing the blk irq capability failed")?;
        if vfs_ep.raw() != 1 || blk_ep.raw() != 1 {
            return Err("storage endpoint handle is not 1");
        }
        crate::kprintln!("meuxe: mmio_cap={}", mmio_handle.raw());
        crate::kprintln!("meuxe: irq cap task={} vector=33", task::BLK);
        Ok(())
    })
}

fn map_window(cr3: u64, virt_base: u64, phys: u64, len: u32) -> Result<u64, &'static str> {
    if len == 0 {
        return Err("mmio window is empty");
    }
    let page = phys & !0xFFF;
    let offset = phys & 0xFFF;
    let span = offset + len as u64;
    let pages = ((span + 4095) / 4096) as usize;
    if pages == 0 || pages > 16 {
        return Err("mmio window is too large");
    }
    for index in 0..pages {
        mm::map_user_in(
            cr3,
            virt_base + (index as u64) * 4096,
            page + (index as u64) * 4096,
            UserPerm::UncachedRw,
        )?;
    }
    Ok(virt_base + offset)
}

fn copy_user(ptr: u64, len: u64) -> Option<[u8; 32]> {
    if len == 0 || len > 32 {
        return None;
    }
    let len = len as usize;
    let cr3 = cpu::read_cr3() & 0x000F_FFFF_FFFF_F000;
    let last = ptr + len as u64 - 1;
    let flags = mm::leaf_user(cr3, ptr)?;
    let last_flags = mm::leaf_user(cr3, last)?;
    if flags & 4 == 0 || last_flags & 4 == 0 {
        return None;
    }
    let mut buf = [0u8; 32];
    for (index, slot) in buf.iter_mut().take(len).enumerate() {
        *slot = unsafe { ((ptr + index as u64) as *const u8).read_volatile() };
    }
    Some(buf)
}

