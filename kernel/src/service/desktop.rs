//! Hand the GOP framebuffer to a ring-3 compositor.
//!
//! A client fills a shared back buffer and sends one dirty rectangle. A
//! virtio-tablet driver scales absolute axes and sends a pointer. The
//! compositor blits the rectangle and hit-tests it. This task reads the
//! framebuffer through the direct map and the status word the compositor
//! stores. IOAPIC lines stay masked. Virtio-blk, the tablet, and the keyboard
//! complete through MSI-X.

use crate::arch::x86_64::cpu;
use crate::boot::{BootInfo, FbInfo};
use crate::cap;
use crate::console;
use crate::ipc;
use crate::exec;
use crate::mm::{self, UserPerm};
use crate::dev::pci;
use crate::sched;
use crate::task;
use meuxe_abi::{
    CalcBoot, ClientBoot, CompositorBoot, FilesBoot, InputBoot, KeyboardBoot, Rights, USER_CHILD,
    USER_IMAGE, USER_IMAGE_BYTES, CALC_COLS,
    CALC_PX_H, CALC_PX_W, CALC_ROWS, CALC_SCALE, CALC_X, CALC_Y, FILES_PX_H, FILES_PX_W, FILES_SCALE,
    FILES_X, FILES_Y, USER_BACK, USER_CALC, USER_CALC_PICK, USER_FB, USER_FILES, USER_FRONT, USER_FS,
    USER_FS_FILES, USER_INFO, USER_KBD_EVENT, USER_KBD_INFO, USER_KBD_QUEUE, USER_KBD_READY,
    USER_MMIO, USER_PICK, USER_QUEUE, USER_SHARE, USER_STATUS, USER_TERM, WINDOW_COLOR, WINDOW_H,
    WINDOW_W, WINDOW_X, WINDOW_Y, FILES_COLS, FILES_ROWS, TERM_COLS, TERM_PX_H, TERM_PX_W, TERM_ROWS,
    TERM_SCALE, TERM_X, TERM_Y,
};
use meuxe_cap::{ObjectKind, TaskId};
use meuxe_fs::Archive;

const INITRAMFS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/initramfs.bin"));

const FB_LIMIT: u64 = 8 * 1024 * 1024;

pub fn start(boot: &BootInfo) -> Result<(), &'static str> {
    console::release();
    let fb = boot.fb.ok_or("framebuffer disappeared before the desktop")?;
    if fb.bpp != 32
        || fb.width < WINDOW_X + WINDOW_W
        || fb.height < WINDOW_Y + WINDOW_H
        || fb.width < TERM_X + TERM_PX_W
        || fb.height < TERM_Y + TERM_PX_H
        || fb.width < FILES_X + FILES_PX_W
        || fb.height < FILES_Y + FILES_PX_H
        || fb.width < CALC_X + CALC_PX_W
        || fb.height < CALC_Y + CALC_PX_H
    {
        return Err("framebuffer cannot hold the windows");
    }
    let bytes = fb.pitch as u64 * fb.height as u64;
    if bytes == 0 || bytes > FB_LIMIT {
        return Err("framebuffer is larger than the desktop map");
    }
    crate::kprintln!("meuxe: desktop fb {}x{}", fb.width, fb.height);

    let archive = Archive::parse(INITRAMFS).map_err(meuxe_fs::FsError::as_str)?;
    let compositor_image = archive
        .lookup(b"compositor")
        .ok_or("initramfs is missing compositor")?;
    let client_image = archive
        .lookup(b"client")
        .ok_or("initramfs is missing client")?;
    let input_image = archive
        .lookup(b"input")
        .ok_or("initramfs is missing input")?;
    let files_image = archive
        .lookup(b"files")
        .ok_or("initramfs is missing files")?;
    let calc_image = archive
        .lookup(b"calc")
        .ok_or("initramfs is missing calc")?;
    let device = pci::find_virtio_tablet()?;
    let keyboard = pci::find_virtio_keyboard()?;
    crate::kprintln!(
        "meuxe: virtio-tablet mmio={:#x} notify={:#x}",
        device.common,
        device.notify
    );
    crate::kprintln!(
        "meuxe: virtio-kbd mmio={:#x} notify={:#x}",
        keyboard.common,
        keyboard.notify
    );

    let compositor = exec::load(compositor_image)?;
    let client = exec::load(client_image)?;
    let input = exec::load(input_image)?;
    let files = exec::load(files_image)?;
    let calc = exec::load(calc_image)?;
    let kernel = mm::kernel_cr3();
    let spaces = [
        compositor.cr3,
        client.cr3,
        input.cr3,
        files.cr3,
        calc.cr3,
        kernel,
    ];
    for (index, cr3) in spaces.iter().enumerate() {
        if spaces[..index].contains(cr3) {
            return Err("desktop address spaces are not distinct");
        }
    }

    let front = mm::alloc_frame_zeroed()?;
    let back = mm::alloc_frame_zeroed()?;
    let status = mm::alloc_frame_zeroed()?;
    let ready = mm::alloc_frame_zeroed()?;
    let kbd_ready = mm::alloc_frame_zeroed()?;
    let queue = mm::alloc_frame_zeroed()?;
    let events = mm::alloc_frame_zeroed()?;
    let kbd_queue = mm::alloc_frame_zeroed()?;
    let kbd_events = mm::alloc_frame_zeroed()?;
    let comp_info = mm::alloc_frame_zeroed()?;
    let client_info = mm::alloc_frame_zeroed()?;
    let input_info = mm::alloc_frame_zeroed()?;
    let files_info = mm::alloc_frame_zeroed()?;
    let calc_info = mm::alloc_frame_zeroed()?;
    let pick = mm::alloc_frame_zeroed()?;
    let calc_pick = mm::alloc_frame_zeroed()?;
    let files_fs = mm::alloc_frame_zeroed()?;

    mm::map_user_in(compositor.cr3, USER_FRONT, front, UserPerm::Rw)?;
    mm::map_user_in(compositor.cr3, USER_BACK, back, UserPerm::Rw)?;
    mm::map_user_in(client.cr3, USER_FRONT, front, UserPerm::Rw)?;
    mm::map_user_in(client.cr3, USER_BACK, back, UserPerm::Rw)?;
    mm::map_user_in(compositor.cr3, USER_STATUS, status, UserPerm::Rw)?;
    let fb_virt = map_framebuffer(compositor.cr3, &fb)?;
    map_term(compositor.cr3, client.cr3)?;
    map_files(compositor.cr3, files.cr3)?;
    map_calc(compositor.cr3, calc.cr3)?;
    mm::map_user_in(compositor.cr3, USER_PICK, pick, UserPerm::Rw)?;
    mm::map_user_in(files.cr3, USER_PICK, pick, UserPerm::Rw)?;
    mm::map_user_in(compositor.cr3, USER_CALC_PICK, calc_pick, UserPerm::Rw)?;
    mm::map_user_in(calc.cr3, USER_CALC_PICK, calc_pick, UserPerm::Rw)?;
    let vfs_cr3 = crate::service::storage::vfs_cr3();
    if vfs_cr3 == 0 {
        return Err("vfs address space is missing");
    }
    mm::map_user_in(vfs_cr3, USER_FS_FILES, files_fs, UserPerm::Rw)?;
    mm::map_user_in(files.cr3, USER_FS, files_fs, UserPerm::Rw)?;

    mm::map_user_in(input.cr3, USER_QUEUE, queue, UserPerm::UncachedRw)?;
    mm::map_user_in(input.cr3, USER_SHARE, events, UserPerm::UncachedRw)?;
    mm::map_user_in(input.cr3, USER_STATUS, ready, UserPerm::Rw)?;
    mm::map_user_in(input.cr3, USER_KBD_QUEUE, kbd_queue, UserPerm::UncachedRw)?;
    mm::map_user_in(input.cr3, USER_KBD_EVENT, kbd_events, UserPerm::UncachedRw)?;
    mm::map_user_in(input.cr3, USER_KBD_READY, kbd_ready, UserPerm::Rw)?;
    let common = map_window(input.cr3, USER_MMIO, device.common, device.common_len.max(0x40))?;
    let notify = map_window(
        input.cr3,
        USER_MMIO + 0x10000,
        device.notify,
        device.notify_len.max(4),
    )?;
    let kbd_common = map_window(
        input.cr3,
        USER_MMIO + 0x30000,
        keyboard.common,
        keyboard.common_len.max(0x40),
    )?;
    let kbd_notify = map_window(
        input.cr3,
        USER_MMIO + 0x40000,
        keyboard.notify,
        keyboard.notify_len.max(4),
    )?;

    let comp_boot = CompositorBoot {
        fb: fb_virt,
        front: USER_FRONT,
        back: USER_BACK,
        status: USER_STATUS,
        width: fb.width,
        height: fb.height,
        pitch: fb.pitch,
        red_shift: fb.red_shift,
        green_shift: fb.green_shift,
        blue_shift: fb.blue_shift,
        _pad: 0,
        term: USER_TERM,
        term_stride: TERM_PX_W,
        term_x: TERM_X,
        term_y: TERM_Y,
        _term_pad: 0,
        files: USER_FILES,
        files_stride: FILES_PX_W,
        files_x: FILES_X,
        files_y: FILES_Y,
        _files_pad: 0,
        pick: USER_PICK,
        calc: USER_CALC,
        calc_stride: CALC_PX_W,
        calc_x: CALC_X,
        calc_y: CALC_Y,
        _calc_pad: 0,
        calc_pick: USER_CALC_PICK,
    };
    let calc_boot = CalcBoot {
        surface: USER_CALC,
        pick: USER_CALC_PICK,
        stride: CALC_PX_W,
        cols: CALC_COLS,
        rows: CALC_ROWS,
        scale: CALC_SCALE,
        origin_x: CALC_X,
        origin_y: CALC_Y,
    };
    let files_boot = FilesBoot {
        surface: USER_FILES,
        pick: USER_PICK,
        stride: FILES_PX_W,
        cols: FILES_COLS,
        rows: FILES_ROWS,
        scale: FILES_SCALE,
        origin_x: FILES_X,
        origin_y: FILES_Y,
    };
    let client_boot = ClientBoot {
        back: USER_BACK,
        width: WINDOW_W,
        height: WINDOW_H,
        origin_x: WINDOW_X,
        origin_y: WINDOW_Y,
        term: USER_TERM,
        term_stride: TERM_PX_W,
        term_x: TERM_X,
        term_y: TERM_Y,
        cols: TERM_COLS,
        rows: TERM_ROWS,
        scale: TERM_SCALE,
        screen_w: fb.width,
        screen_h: fb.height,
    };
    let input_boot = InputBoot {
        common,
        notify,
        notify_mul: device.notify_mul,
        width: fb.width,
        height: fb.height,
        _pad: 0,
        queue_phys: queue,
        event_phys: events,
        queue_virt: USER_QUEUE,
        event_virt: USER_SHARE,
        ready: USER_STATUS,
    };
    let kbd_boot = KeyboardBoot {
        common: kbd_common,
        notify: kbd_notify,
        notify_mul: keyboard.notify_mul,
        _pad: 0,
        queue_phys: kbd_queue,
        event_phys: kbd_events,
        queue_virt: USER_KBD_QUEUE,
        event_virt: USER_KBD_EVENT,
        ready: USER_KBD_READY,
    };
    unsafe {
        ((mm::hhdm() + comp_info) as *mut CompositorBoot).write_volatile(comp_boot);
        ((mm::hhdm() + client_info) as *mut ClientBoot).write_volatile(client_boot);
        ((mm::hhdm() + input_info) as *mut InputBoot).write_volatile(input_boot);
        ((mm::hhdm() + input_info + (USER_KBD_INFO - USER_INFO)) as *mut KeyboardBoot)
            .write_volatile(kbd_boot);
        ((mm::hhdm() + files_info) as *mut FilesBoot).write_volatile(files_boot);
        ((mm::hhdm() + calc_info) as *mut CalcBoot).write_volatile(calc_boot);
    }
    mm::map_user_in(compositor.cr3, USER_INFO, comp_info, UserPerm::Ro)?;
    mm::map_user_in(client.cr3, USER_INFO, client_info, UserPerm::Ro)?;
    mm::map_user_in(input.cr3, USER_INFO, input_info, UserPerm::Ro)?;
    mm::map_user_in(files.cr3, USER_INFO, files_info, UserPerm::Ro)?;
    mm::map_user_in(calc.cr3, USER_INFO, calc_info, UserPerm::Ro)?;
    let fs = crate::service::storage::fs_frame();
    if fs == 0 {
        return Err("filesystem page is missing");
    }
    mm::map_user_in(client.cr3, USER_FS, fs, UserPerm::Rw)?;
    let child_share = mm::alloc_frame_zeroed()?;
    mm::map_user_in(client.cr3, USER_CHILD, child_share, UserPerm::Rw)?;
    crate::proc::init_terminal(child_share);
    let image_pages = (USER_IMAGE_BYTES / 4096) as usize;
    for index in 0..image_pages {
        let frame = mm::alloc_frame_zeroed()?;
        mm::map_user_in(client.cr3, USER_IMAGE + (index as u64) * 4096, frame, UserPerm::Rw)?;
    }

    install_caps(fb.phys, bytes)?;
    ipc::register_ring(task::COMPOSITOR, compositor.ring_phys);
    ipc::register_ring(task::TERMINAL, client.ring_phys);
    ipc::register_ring(task::INPUT, input.ring_phys);
    ipc::register_ring(task::FILES, files.ring_phys);
    ipc::register_ring(task::CALC, calc.ring_phys);
    sched::spawn_user_elf(task::COMPOSITOR, compositor.entry, compositor.cr3);
    sched::spawn_user_elf(task::TERMINAL, client.entry, client.cr3);
    sched::spawn_user_elf(task::INPUT, input.entry, input.cr3);
    sched::spawn_user_elf(task::FILES, files.entry, files.cr3);
    sched::spawn_user_elf(task::CALC, calc.entry, calc.cr3);
    crate::kprintln!("meuxe: files window");
    crate::kprintln!("meuxe: calc task={}", task::CALC);
    crate::kprintln!("meuxe: calc window");

    if !cfg!(feature = "verify") {
        return Ok(());
    }

    let encoded = encode(&fb, WINDOW_COLOR);
    let start = sched::ticks();
    let mut listening = false;
    let mut saw_miss = false;
    while sched::ticks().wrapping_sub(start) <= 4000 {
        if !listening {
            let flag = unsafe { ((mm::hhdm() + ready) as *const u8).read_volatile() };
            if flag == 2 {
                return Err("virtio-tablet setup failed");
            }
            if flag == 1 {
                crate::kprintln!("meuxe: tablet listening");
                listening = true;
            }
        }
        crate::verify::watch(kbd_ready);
        let hit = unsafe { ((mm::hhdm() + status) as *const u32).read_volatile() };
        if hit == 2 {
            saw_miss = true;
        }
        if hit == 1 {
            cpu::mfence();
            let px = unsafe { ((mm::hhdm() + status) as *const u32).add(1).read_volatile() };
            let py = unsafe { ((mm::hhdm() + status) as *const u32).add(2).read_volatile() };
            if px < WINDOW_X || py < WINDOW_Y || px >= WINDOW_X + WINDOW_W || py >= WINDOW_Y + WINDOW_H
            {
                crate::kprintln!("meuxe: desktop hit=1 x={px} y={py}");
                return Err("hit is outside the window");
            }
            let pixel = read_pixel(&fb, WINDOW_X, WINDOW_Y);
            if pixel != encoded {
                crate::kprintln!("meuxe: desktop pixel={pixel:#x}");
                return Err("framebuffer pixel mismatch");
            }
            crate::kprintln!("meuxe: desktop pixel=0xe07a3d");
            crate::kprintln!("meuxe: desktop hit=1 x={px} y={py}");
            return Ok(());
        }
        cpu::hlt();
    }
    if saw_miss {
        Err("pointer missed the window")
    } else if !listening {
        Err("input driver did not post buffers")
    } else {
        Err("compositor did not report a hit")
    }
}

fn install_caps(fb_phys: u64, fb_len: u64) -> Result<(), &'static str> {
    let compositor = TaskId::new(task::COMPOSITOR).ok_or("compositor task id is out of range")?;
    let client = TaskId::new(task::TERMINAL).ok_or("client task id is out of range")?;
    let input = TaskId::new(task::INPUT).ok_or("input task id is out of range")?;
    cap::with_mut(|caps| {
        let to_client = caps
            .create(ObjectKind::Endpoint { parked: None })
            .map_err(|_| "endpoint table is full")?;
        let to_input = caps
            .create(ObjectKind::Endpoint { parked: None })
            .map_err(|_| "endpoint table is full")?;
        let to_keys = caps
            .create(ObjectKind::Endpoint { parked: None })
            .map_err(|_| "endpoint table is full")?;
        let mmio = caps
            .create(ObjectKind::Mmio {
                phys: fb_phys,
                len: fb_len,
            })
            .map_err(|_| "mmio table is full")?;
        let rw = Rights::READ.union(Rights::WRITE);
        let client_ep = caps
            .install(client, to_client, rw)
            .map_err(|_| "installing the client endpoint failed")?;
        let comp_client = caps
            .install(compositor, to_client, rw)
            .map_err(|_| "installing the compositor client endpoint failed")?;
        let input_ep = caps
            .install(input, to_input, rw)
            .map_err(|_| "installing the input endpoint failed")?;
        let comp_input = caps
            .install(compositor, to_input, rw)
            .map_err(|_| "installing the compositor input endpoint failed")?;
        let input_keys = caps
            .install(input, to_keys, rw)
            .map_err(|_| "installing the keyboard endpoint failed")?;
        let client_keys = caps
            .install(client, to_keys, rw)
            .map_err(|_| "installing the terminal keyboard endpoint failed")?;
        let vfs = TaskId::new(task::VFS).ok_or("vfs task id is out of range")?;
        let fs_end = caps
            .create(ObjectKind::Endpoint { parked: None })
            .map_err(|_| "filesystem endpoint table is full")?;
        let vfs_fs = caps
            .install(vfs, fs_end, rw)
            .map_err(|_| "installing the vfs directory endpoint failed")?;
        let client_fs = caps
            .install(client, fs_end, rw)
            .map_err(|_| "installing the shell directory endpoint failed")?;
        let fb_handle = caps
            .install(compositor, mmio, rw)
            .map_err(|_| "installing the framebuffer capability failed")?;
        let files = TaskId::new(task::FILES).ok_or("files task id is out of range")?;
        let to_files = caps
            .create(ObjectKind::Endpoint { parked: None })
            .map_err(|_| "endpoint table is full")?;
        let files_ep = caps
            .install(files, to_files, rw)
            .map_err(|_| "installing the files endpoint failed")?;
        let comp_files = caps
            .install(compositor, to_files, rw)
            .map_err(|_| "installing the compositor files endpoint failed")?;
        let files_dir = caps
            .create(ObjectKind::Endpoint { parked: None })
            .map_err(|_| "filesystem endpoint table is full")?;
        let vfs_files = caps
            .install(vfs, files_dir, rw)
            .map_err(|_| "installing the files directory endpoint failed")?;
        let files_fs = caps
            .install(files, files_dir, rw)
            .map_err(|_| "installing the files vfs endpoint failed")?;
        if client_ep.raw() != 1 || input_ep.raw() != 1 {
            return Err("peer endpoint handle is not 1");
        }
        if comp_client.raw() != 1 || comp_input.raw() != 2 || fb_handle.raw() != 3 {
            return Err("compositor handles are not 1, 2, and 3");
        }
        if input_keys.raw() != 2 || client_keys.raw() != 2 {
            return Err("keyboard endpoint handle is not 2");
        }
        if vfs_fs.raw() != 3 || client_fs.raw() != 3 {
            return Err("filesystem endpoint handle is not 3");
        }
        if files_ep.raw() != 1 || files_fs.raw() != 2 {
            return Err("files handles are not 1 and 2");
        }
        if comp_files.raw() != 4 || vfs_files.raw() != 4 {
            return Err("files peer handle is not 4");
        }
        let calc_task = TaskId::new(task::CALC).ok_or("calc task id is out of range")?;
        let to_calc = caps
            .create(ObjectKind::Endpoint { parked: None })
            .map_err(|_| "endpoint table is full")?;
        let calc_ep = caps
            .install(calc_task, to_calc, rw)
            .map_err(|_| "installing the calc endpoint failed")?;
        let comp_calc = caps
            .install(compositor, to_calc, rw)
            .map_err(|_| "installing the compositor calc endpoint failed")?;
        if calc_ep.raw() != 1 || comp_calc.raw() != 5 {
            return Err("calc handles are not 1 and 5");
        }
        let tablet_irq = caps
            .create(ObjectKind::Irq { vector: 34 })
            .map_err(|_| "tablet irq object table is full")?;
        let kbd_irq = caps
            .create(ObjectKind::Irq { vector: 35 })
            .map_err(|_| "keyboard irq object table is full")?;
        caps.install(input, tablet_irq, Rights::READ)
            .map_err(|_| "installing the tablet irq capability failed")?;
        caps.install(input, kbd_irq, Rights::READ)
            .map_err(|_| "installing the keyboard irq capability failed")?;
        Ok(())
    })
}

fn map_calc(compositor: u64, calc: u64) -> Result<(), &'static str> {
    let bytes = CALC_PX_W as u64 * CALC_PX_H as u64 * 4;
    let pages = (bytes / 4096) as usize;
    if pages == 0 || bytes != pages as u64 * 4096 {
        return Err("calc buffer is not page-aligned");
    }
    for index in 0..pages {
        let frame = mm::alloc_frame_zeroed()?;
        let virt = USER_CALC + (index as u64) * 4096;
        mm::map_user_in(compositor, virt, frame, UserPerm::Rw)?;
        mm::map_user_in(calc, virt, frame, UserPerm::Rw)?;
    }
    Ok(())
}

fn map_files(compositor: u64, files: u64) -> Result<(), &'static str> {
    let bytes = FILES_PX_W as u64 * FILES_PX_H as u64 * 4;
    let pages = (bytes / 4096) as usize;
    if pages == 0 || bytes != pages as u64 * 4096 {
        return Err("files buffer is not page-aligned");
    }
    for index in 0..pages {
        let frame = mm::alloc_frame_zeroed()?;
        let virt = USER_FILES + (index as u64) * 4096;
        mm::map_user_in(compositor, virt, frame, UserPerm::Rw)?;
        mm::map_user_in(files, virt, frame, UserPerm::Rw)?;
    }
    Ok(())
}

fn map_term(compositor: u64, client: u64) -> Result<(), &'static str> {
    let bytes = TERM_PX_W as u64 * TERM_PX_H as u64 * 4;
    let pages = (bytes / 4096) as usize;
    if pages == 0 || bytes != pages as u64 * 4096 {
        return Err("terminal buffer is not page-aligned");
    }
    for index in 0..pages {
        let frame = mm::alloc_frame_zeroed()?;
        let virt = USER_TERM + (index as u64) * 4096;
        mm::map_user_in(compositor, virt, frame, UserPerm::Rw)?;
        mm::map_user_in(client, virt, frame, UserPerm::Rw)?;
    }
    Ok(())
}

fn map_framebuffer(cr3: u64, fb: &FbInfo) -> Result<u64, &'static str> {
    let bytes = fb.pitch as u64 * fb.height as u64;
    let page = fb.phys & !0xFFF;
    let offset = fb.phys & 0xFFF;
    let pages = ((offset + bytes + 4095) / 4096) as usize;
    if pages == 0 || pages > 2048 {
        return Err("framebuffer page count is out of range");
    }
    for index in 0..pages {
        mm::map_user_in(
            cr3,
            USER_FB + (index as u64) * 4096,
            page + (index as u64) * 4096,
            UserPerm::Rw,
        )?;
    }
    Ok(USER_FB + offset)
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

fn encode(fb: &FbInfo, rgb: u32) -> u32 {
    let red = (rgb >> 16) & 0xFF;
    let green = (rgb >> 8) & 0xFF;
    let blue = rgb & 0xFF;
    (red << fb.red_shift) | (green << fb.green_shift) | (blue << fb.blue_shift)
}

fn read_pixel(fb: &FbInfo, x: u32, y: u32) -> u32 {
    let offset = y as usize * fb.pitch as usize + x as usize * 4;
    unsafe {
        (fb.virt as *const u8)
            .add(offset)
            .cast::<u32>()
            .read_volatile()
    }
}
