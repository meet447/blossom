//! Virtio-net driver and TCP/ICMP stack server.

use crate::cap;
use crate::dev::pci;
use crate::exec;
use crate::ipc;
use crate::mm::{self, UserPerm};
use crate::sched;
use crate::task;
use core::sync::atomic::{AtomicU64, Ordering};
use meuxe_abi::{NetBoot, Rights, USER_INFO, USER_MMIO, USER_NET, USER_NET_DMA};
use meuxe_cap::{ObjectKind, TaskId};
use meuxe_fs::Archive;

const INITRAMFS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/initramfs.bin"));
const DMA_PAGES: usize = 16;
const TICK_DMA_PAGE: u64 = 14;

static TICK_PAGE: AtomicU64 = AtomicU64::new(0);

pub fn tick_page() -> u64 {
    TICK_PAGE.load(Ordering::Acquire)
}

pub fn start(client_cr3: u64) -> Result<(), &'static str> {
    let archive = Archive::parse(INITRAMFS).map_err(meuxe_fs::FsError::as_str)?;
    let net_image = archive.lookup(b"net").ok_or("initramfs is missing net")?;
    let device = pci::find_virtio_net()?;
    crate::kprintln!(
        "meuxe: virtio-net mmio={:#x} notify={:#x} isr={:#x}/{}",
        device.common,
        device.notify,
        device.isr,
        device.isr_len
    );

    let net = exec::load(net_image)?;
    if net.cr3 == mm::kernel_cr3() || net.cr3 == client_cr3 {
        return Err("net address space is not distinct");
    }
    crate::kprintln!(
        "meuxe: elf=net entry={:#x} cr3={:#x}",
        net.entry,
        net.cr3
    );

    let share = mm::alloc_frame_zeroed()?;
    let info = mm::alloc_frame_zeroed()?;
    let tick = mm::alloc_frame_zeroed()?;
    TICK_PAGE.store(tick, Ordering::Release);

    let mut dma_phys = [0u64; 16];
    for index in 0..DMA_PAGES {
        let frame = mm::alloc_frame_zeroed()?;
        dma_phys[index] = frame;
        let virt = USER_NET_DMA + (index as u64) * 4096;
        mm::map_user_in(net.cr3, virt, frame, UserPerm::UncachedRw)?;
    }

    mm::map_user_in(net.cr3, USER_NET, share, UserPerm::Rw)?;
    mm::map_user_in(client_cr3, USER_NET, share, UserPerm::Rw)?;
    mm::map_user_in(net.cr3, USER_NET_DMA + TICK_DMA_PAGE * 4096, tick, UserPerm::Ro)?;

    let common = map_window(net.cr3, USER_MMIO, device.common, device.common_len.max(0x40))?;
    let notify = map_window(
        net.cr3,
        USER_MMIO + 0x10000,
        device.notify,
        device.notify_len.max(4),
    )?;
    let device_va = if device.device == 0 {
        0
    } else {
        map_window(
            net.cr3,
            USER_MMIO + 0x20000,
            device.device,
            device.device_len.max(8),
        )?
    };
    let isr_va = if device.isr == 0 {
        0
    } else {
        map_window(
            net.cr3,
            USER_MMIO + 0x30000,
            device.isr,
            device.isr_len.max(1),
        )?
    };

    let boot = NetBoot {
        common,
        notify,
        device: device_va,
        notify_mul: device.notify_mul,
        queue_size: 6,
        dma_phys,
        dma_virt: USER_NET_DMA,
        share_phys: share,
        share_virt: USER_NET,
        tick_phys: tick,
        tick_virt: USER_NET_DMA + TICK_DMA_PAGE * 4096,
        isr: isr_va,
    };
    unsafe {
        ((mm::hhdm() + info) as *mut NetBoot).write_volatile(boot);
    }
    mm::map_user_in(net.cr3, USER_INFO, info, UserPerm::Ro)?;

    install_caps(share, device.common)?;
    ipc::register_ring(task::NET, net.ring_phys);
    sched::spawn_user_elf(task::NET, net.entry, net.cr3);
    Ok(())
}

fn install_caps(share: u64, mmio_phys: u64) -> Result<(), &'static str> {
    let net = TaskId::new(task::NET).ok_or("net task id is out of range")?;
    let client = TaskId::new(task::TERMINAL).ok_or("client task id is out of range")?;
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
        let net_ep = caps
            .install(net, endpoint, rw)
            .map_err(|_| "installing the net endpoint failed")?;
        let client_ep = caps
            .install(client, endpoint, rw)
            .map_err(|_| "installing the terminal net endpoint failed")?;
        caps.install(net, frame, rw)
            .map_err(|_| "installing the net frame failed")?;
        let mmio_handle = caps
            .install(net, mmio, rw)
            .map_err(|_| "installing the net mmio capability failed")?;
        let irq = caps
            .create(ObjectKind::Irq { vector: 36 })
            .map_err(|_| "irq object table is full")?;
        caps.install(net, irq, Rights::READ)
            .map_err(|_| "installing the net irq capability failed")?;
        if net_ep.raw() != 1 || client_ep.raw() != 4 {
            return Err("net endpoint handles are not 1 and 4");
        }
        if mmio_handle.raw() != 3 {
            return Err("net mmio handle is not 3");
        }
        crate::kprintln!("meuxe: irq cap task={} vector=36", task::NET);
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
