//! Physical frames and the kernel heap.
//!
//! The bitmap tracks at most 16 GiB. Usable Limine entries are freed; the
//! kernel image, framebuffer, and APIC pages are then reserved again.

pub mod layout;
mod vmm;

use crate::boot::{self, BootInfo};
use crate::sync::Mutex;
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, Ordering};
use meuxe_mm::{BitmapFrameAllocator, LinkedHeap, PageSize, PhysicalAddress, PAGE_SIZE};

pub use layout::{data, rodata, text, HEAP_VIRT};
pub use vmm::{
    activate_and_record, hhdm, kernel_cr3, leaf_flags, leaf_user, map_kernel, map_kernel_mmio,
    map_user_4k, map_user_in, new_address_space, UserPerm,
};

const MAX_PHYS: u64 = 16 * 1024 * 1024 * 1024;
const MAX_FRAMES: usize = (MAX_PHYS / PAGE_SIZE) as usize;
const BITMAP_WORDS: usize = MAX_FRAMES / 64;

static mut BITMAP: [u64; BITMAP_WORDS] = [0; BITMAP_WORDS];
static mut FRAMES: core::mem::MaybeUninit<BitmapFrameAllocator<'static>> =
    core::mem::MaybeUninit::uninit();
static FRAMES_READY: AtomicBool = AtomicBool::new(false);

struct KernelAlloc;

static HEAP: Mutex<Option<LinkedHeap>> = Mutex::new(None);

#[global_allocator]
static GLOBAL: KernelAlloc = KernelAlloc;

unsafe impl GlobalAlloc for KernelAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match HEAP.lock().as_mut() {
            Some(heap) => heap.alloc(layout),
            None => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if let Some(heap) = HEAP.lock().as_mut() {
            heap.dealloc(ptr);
        }
    }
}

pub fn init_frames(boot: &BootInfo) -> Result<(), &'static str> {
    let mut highest = 0u64;
    for region in boot.regions() {
        let Some(end) = region.base.checked_add(region.length) else {
            return Err("memory region overflows");
        };
        highest = highest.max(end);
    }
    highest = highest.min(MAX_PHYS);
    if highest == 0 {
        return Err("memory map is empty");
    }
    let mut frames = ((highest + PAGE_SIZE - 1) / PAGE_SIZE) as usize;
    frames = frames.div_ceil(512) * 512;
    if frames > MAX_FRAMES {
        frames = MAX_FRAMES;
    }
    let storage = unsafe { &mut *core::ptr::addr_of_mut!(BITMAP) };
    let mut alloc = BitmapFrameAllocator::new(PhysicalAddress::new(0), frames, storage)
        .map_err(|_| "frame bitmap rejected the memory map")?;
    for region in boot.regions() {
        if region.kind == boot::MEMMAP_USABLE {
            alloc
                .mark_free(PhysicalAddress::new(region.base), region.length)
                .map_err(|_| "marking usable memory failed")?;
        }
    }
    let image = boot::kernel_image_bytes(boot);
    alloc
        .mark_used(PhysicalAddress::new(boot.kernel_phys), image)
        .map_err(|_| "reserving the kernel image failed")?;
    if let Some(fb) = boot.fb {
        let bytes = fb.height as u64 * fb.pitch as u64;
        alloc
            .mark_used(PhysicalAddress::new(fb.phys), bytes)
            .map_err(|_| "reserving the framebuffer failed")?;
    }
    alloc
        .mark_used(PhysicalAddress::new(boot.lapic_phys), PAGE_SIZE)
        .map_err(|_| "reserving the local apic failed")?;
    for apic in boot.ioapics.iter().take(boot.ioapic_count) {
        alloc
            .mark_used(PhysicalAddress::new(apic.phys), PAGE_SIZE)
            .map_err(|_| "reserving an ioapic failed")?;
    }
    unsafe {
        core::ptr::addr_of_mut!(FRAMES).write(core::mem::MaybeUninit::new(alloc));
    }
    FRAMES_READY.store(true, Ordering::Release);
    Ok(())
}

pub fn frames() -> &'static mut BitmapFrameAllocator<'static> {
    assert!(FRAMES_READY.load(Ordering::Acquire));
    unsafe { (*core::ptr::addr_of_mut!(FRAMES)).assume_init_mut() }
}

pub fn alloc_frame_zeroed() -> Result<u64, &'static str> {
    let frame = frames()
        .allocate(PageSize::Size4KiB)
        .map_err(|_| "no frame for a user page")?;
    let phys = frame.as_u64();
    unsafe {
        core::ptr::write_bytes((hhdm() + phys) as *mut u8, 0, PAGE_SIZE as usize);
    }
    Ok(phys)
}

pub fn free_frames() -> usize {
    frames().free_frames()
}

pub fn probe_huge_frame() -> Result<u64, &'static str> {
    let alloc = frames();
    let frame = alloc
        .allocate(PageSize::Size2MiB)
        .map_err(|_| "no free 2 MiB frame")?;
    let addr = frame.as_u64();
    if !frame.is_aligned(2 * 1024 * 1024) {
        return Err("2 MiB frame is not aligned");
    }
    alloc
        .free(frame, PageSize::Size2MiB)
        .map_err(|_| "freeing the 2 MiB frame failed")?;
    Ok(addr)
}

pub fn init_heap(virt: u64, size: usize) -> Result<(), &'static str> {
    let heap = unsafe { LinkedHeap::init(virt as *mut u8, size).map_err(|_| "heap alignment")? };
    *HEAP.lock() = Some(heap);
    Ok(())
}
