//! Higher-half 4-level page tables.
//!
//! The direct map keeps Limine's HHDM offset, so the stack and bootloader
//! responses stay valid across the CR3 switch. Kernel sections are mapped
//! again with strict flags: executable code is read-only, everything else
//! is no-execute. The GOP framebuffer is ordinary RAM, so it is mapped
//! write-back with the rest of the direct map and a pixel readback is valid.

use super::layout::{
    self, HEAP_SIZE, HEAP_VIRT, IOAPIC_STRIDE, IOAPIC_VIRT, LAPIC_VIRT, MSI_DEVICES, MSI_STRIDE,
    MSI_VIRT,
};
use crate::arch::x86_64::cpu;
use crate::boot::{
    BootInfo, MEMMAP_ACPI_NVS, MEMMAP_ACPI_RECLAIMABLE, MEMMAP_BOOTLOADER_RECLAIMABLE,
    MEMMAP_BAD_MEMORY, MEMMAP_EXECUTABLE_AND_MODULES, MEMMAP_FRAMEBUFFER, MEMMAP_RESERVED,
    MEMMAP_RESERVED_MAPPED, MEMMAP_USABLE,
};
use core::sync::atomic::{AtomicU64, Ordering};
use meuxe_mm::{BitmapFrameAllocator, PageSize, PagingLevel4, VirtualAddress, PAGE_SIZE};

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
const HUGE: u64 = 1 << 7;
const GLOBAL: u64 = 1 << 8;
const NX: u64 = 1 << 63;
const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const HUGE_PAGE: u64 = 2 * 1024 * 1024;

const KERNEL_RX: u64 = PRESENT | GLOBAL;
const KERNEL_RO: u64 = PRESENT | GLOBAL | NX;
const KERNEL_RW: u64 = PRESENT | WRITABLE | GLOBAL | NX;
const DIRECT_RW: u64 = PRESENT | WRITABLE | GLOBAL | NX;
const MMIO_UC: u64 = PRESENT | WRITABLE | GLOBAL | NX | PCD | PWT;

static PML4: AtomicU64 = AtomicU64::new(0);
static HHDM: AtomicU64 = AtomicU64::new(0);
static USE_NX: AtomicU64 = AtomicU64::new(0);

struct Mapper<'a> {
    alloc: &'a mut BitmapFrameAllocator<'static>,
    hhdm: u64,
    pml4: u64,
    nx: bool,
}

pub fn map_kernel(
    boot: &BootInfo,
    alloc: &mut BitmapFrameAllocator<'static>,
) -> Result<u64, &'static str> {
    let nx = cpu::nx_enabled();
    if !nx {
        return Err("cpu is missing efer.nxe");
    }
    let highest = boot
        .regions()
        .iter()
        .filter_map(|region| region.base.checked_add(region.length))
        .max()
        .unwrap_or(0);
    ensure_windows_fit(boot.hhdm, highest)?;

    let frame = alloc
        .allocate(PageSize::Size4KiB)
        .map_err(|_| "no frame for pml4")?;
    let pml4 = frame.as_u64();
    unsafe {
        core::ptr::write_bytes((boot.hhdm + pml4) as *mut u8, 0, PAGE_SIZE as usize);
    }
    let mut mapper = Mapper {
        alloc,
        hhdm: boot.hhdm,
        pml4,
        nx,
    };
    mapper.map_image(boot)?;
    for region in boot.regions() {
        let (flags, huge) = match region.kind {
            MEMMAP_FRAMEBUFFER
            | MEMMAP_USABLE
            | MEMMAP_ACPI_RECLAIMABLE
            | MEMMAP_ACPI_NVS
            | MEMMAP_BOOTLOADER_RECLAIMABLE
            | MEMMAP_EXECUTABLE_AND_MODULES
            | MEMMAP_RESERVED_MAPPED => (DIRECT_RW, true),
            MEMMAP_RESERVED | MEMMAP_BAD_MEMORY => continue,
            _ => continue,
        };
        mapper.map_direct(region.base, region.length, flags, huge)?;
    }
    mapper.map_4k(boot.lapic_virt, boot.lapic_phys, MMIO_UC)?;
    for apic in boot.ioapics.iter().take(boot.ioapic_count) {
        mapper.map_4k(apic.virt, apic.phys, MMIO_UC)?;
    }
    mapper.map_heap()?;

    PML4.store(pml4, Ordering::Release);
    HHDM.store(boot.hhdm, Ordering::Release);
    USE_NX.store(1, Ordering::Release);
    Ok(pml4)
}

pub fn activate_and_record(pml4: u64) {
    cpu::write_cr3(pml4);
}

const USER: u64 = 1 << 2;
const USER_RX: u64 = PRESENT | USER;
const USER_RW: u64 = PRESENT | WRITABLE | USER | NX;
const USER_RO: u64 = PRESENT | USER | NX;
const USER_UC_RW: u64 = PRESENT | WRITABLE | USER | NX | PCD | PWT;
const USER_UC_RO: u64 = PRESENT | USER | NX | PCD | PWT;

#[derive(Clone, Copy)]
pub enum UserPerm {
    Rx,
    Rw,
    Ro,
    UncachedRw,
    #[allow(dead_code)]
    UncachedRo,
}

pub fn kernel_cr3() -> u64 {
    PML4.load(Ordering::Acquire)
}

/// A new PML4 that shares the kernel's upper-half tables and has an empty user half.
pub fn new_address_space() -> Result<u64, &'static str> {
    let phys = super::alloc_frame_zeroed()?;
    let kernel = kernel_cr3();
    if kernel == 0 {
        return Err("kernel page tables are not installed");
    }
    unsafe {
        let src = (hhdm() + kernel) as *const u64;
        let dst = (hhdm() + phys) as *mut u64;
        core::ptr::copy_nonoverlapping(src.add(256), dst.add(256), 256);
    }
    Ok(phys)
}

/// Map one uncached kernel page. `phys` is rounded down to a frame.
pub fn map_kernel_mmio(virt: u64, phys: u64) -> Result<(), &'static str> {
    let hhdm = HHDM.load(Ordering::Acquire);
    let pml4 = PML4.load(Ordering::Acquire);
    if hhdm == 0 || pml4 == 0 {
        return Err("kernel page tables are not installed");
    }
    let mut mapper = Mapper {
        alloc: super::frames(),
        hhdm,
        pml4,
        nx: USE_NX.load(Ordering::Acquire) != 0,
    };
    mapper.map_4k(virt, phys & !0xFFF, MMIO_UC)
}

pub fn map_user_in(pml4: u64, virt: u64, phys: u64, perm: UserPerm) -> Result<(), &'static str> {
    let hhdm = HHDM.load(Ordering::Acquire);
    if pml4 == 0 || hhdm == 0 {
        return Err("page tables are not installed");
    }
    let flags = match perm {
        UserPerm::Rx => USER_RX,
        UserPerm::Rw => USER_RW,
        UserPerm::Ro => USER_RO,
        UserPerm::UncachedRw => USER_UC_RW,
        UserPerm::UncachedRo => USER_UC_RO,
    };
    let mut mapper = Mapper {
        alloc: super::frames(),
        hhdm,
        pml4,
        nx: USE_NX.load(Ordering::Acquire) != 0,
    };
    mapper.map_user(virt, phys, flags)
}

pub fn leaf_user(cr3: u64, virt: u64) -> Option<u64> {
    let hhdm = HHDM.load(Ordering::Acquire);
    if cr3 == 0 {
        return None;
    }
    let va = VirtualAddress::<PagingLevel4>::try_new(virt).ok()?;
    let e4 = read_entry(hhdm, cr3 & PHYS_MASK, va.pml4_index())?;
    if e4 & HUGE != 0 {
        return Some(e4);
    }
    let e3 = read_entry(hhdm, e4 & PHYS_MASK, va.pdpt_index())?;
    if e3 & HUGE != 0 {
        return Some(e3);
    }
    let e2 = read_entry(hhdm, e3 & PHYS_MASK, va.pd_index())?;
    if e2 & HUGE != 0 {
        return Some(e2);
    }
    read_entry(hhdm, e2 & PHYS_MASK, va.pt_index())
}

pub fn map_user_4k(virt: u64, phys: u64, writable: bool) -> Result<(), &'static str> {
    let pml4 = PML4.load(Ordering::Acquire);
    let hhdm = HHDM.load(Ordering::Acquire);
    if pml4 == 0 {
        return Err("page tables are not installed");
    }
    let mut mapper = Mapper {
        alloc: super::frames(),
        hhdm,
        pml4,
        nx: USE_NX.load(Ordering::Acquire) != 0,
    };
    let flags = if writable { USER_RW } else { USER_RX };
    mapper.map_user(virt, phys, flags)
}

pub fn hhdm() -> u64 {
    HHDM.load(Ordering::Acquire)
}

pub fn leaf_flags(virt: u64) -> Option<u64> {
    let pml4 = PML4.load(Ordering::Acquire);
    let hhdm = HHDM.load(Ordering::Acquire);
    if pml4 == 0 {
        return None;
    }
    let va = VirtualAddress::<PagingLevel4>::try_new(virt).ok()?;
    let e4 = read_entry(hhdm, pml4, va.pml4_index())?;
    if e4 & HUGE != 0 {
        return Some(e4);
    }
    let e3 = read_entry(hhdm, e4 & PHYS_MASK, va.pdpt_index())?;
    if e3 & HUGE != 0 {
        return Some(e3);
    }
    let e2 = read_entry(hhdm, e3 & PHYS_MASK, va.pd_index())?;
    if e2 & HUGE != 0 {
        return Some(e2);
    }
    read_entry(hhdm, e2 & PHYS_MASK, va.pt_index())
}

fn read_entry(hhdm: u64, table: u64, index: usize) -> Option<u64> {
    let entry = unsafe { ((hhdm + table) as *const u64).add(index).read_volatile() };
    if entry & PRESENT == 0 {
        None
    } else {
        Some(entry)
    }
}

fn ensure_windows_fit(hhdm: u64, highest_phys: u64) -> Result<(), &'static str> {
    let Some(hhdm_end) = hhdm.checked_add(highest_phys) else {
        return Err("hhdm window overflows");
    };
    let reserved = [
        (0xFFFF_FFFF_8000_0000, u64::MAX),
        (HEAP_VIRT, HEAP_VIRT + HEAP_SIZE),
        (LAPIC_VIRT, LAPIC_VIRT + PAGE_SIZE),
        (IOAPIC_VIRT, IOAPIC_VIRT + 8 * IOAPIC_STRIDE),
        (MSI_VIRT, MSI_VIRT + MSI_DEVICES * MSI_STRIDE),
    ];
    for (start, end) in reserved {
        if hhdm < end && start < hhdm_end {
            return Err("hhdm collides with a reserved kernel window");
        }
    }
    Ok(())
}

impl Mapper<'_> {
    fn flags(&self, flags: u64) -> u64 {
        if self.nx {
            flags
        } else {
            flags & !NX
        }
    }

    fn map_image(&mut self, boot: &BootInfo) -> Result<(), &'static str> {
        self.map_section(boot, layout::text(), KERNEL_RX)?;
        self.map_section(boot, layout::rodata(), KERNEL_RO)?;
        self.map_section(boot, layout::data(), KERNEL_RW)?;
        self.map_section(boot, layout::bss(), KERNEL_RW)?;
        self.map_section(boot, layout::got(), KERNEL_RW)?;
        Ok(())
    }

    fn map_section(
        &mut self,
        boot: &BootInfo,
        (start, end): (u64, u64),
        flags: u64,
    ) -> Result<(), &'static str> {
        if start >= end {
            return Ok(());
        }
        let mut virt = align_down(start, PAGE_SIZE);
        let end = align_up(end, PAGE_SIZE);
        while virt < end {
            let delta = virt
                .checked_sub(boot.kernel_virt)
                .ok_or("kernel section is below the limine virtual base")?;
            let phys = boot
                .kernel_phys
                .checked_add(delta)
                .ok_or("kernel physical address overflow")?;
            self.map_4k(virt, phys, flags)?;
            virt += PAGE_SIZE;
        }
        Ok(())
    }

    fn map_direct(
        &mut self,
        base: u64,
        length: u64,
        flags: u64,
        allow_huge: bool,
    ) -> Result<(), &'static str> {
        if length == 0 {
            return Ok(());
        }
        let Some(raw_end) = base.checked_add(length) else {
            return Err("direct map region overflows");
        };
        let mut phys = align_down(base, PAGE_SIZE);
        let end = align_up(raw_end, PAGE_SIZE);
        while phys < end {
            let Some(virt) = self.hhdm.checked_add(phys) else {
                return Err("direct map virtual address overflows");
            };
            let huge_ok = allow_huge && phys % HUGE_PAGE == 0 && phys + HUGE_PAGE <= end;
            if huge_ok {
                match self.map_2m(virt, phys, flags)? {
                    HugeMap::Mapped | HugeMap::AlreadyPresent => {
                        phys += HUGE_PAGE;
                        continue;
                    }
                    HugeMap::Split => {}
                }
            }
            self.map_4k(virt, phys, flags)?;
            phys += PAGE_SIZE;
        }
        Ok(())
    }

    fn map_heap(&mut self) -> Result<(), &'static str> {
        let pages = HEAP_SIZE / HUGE_PAGE;
        for index in 0..pages {
            let frame = self
                .alloc
                .allocate(PageSize::Size2MiB)
                .map_err(|_| "heap could not allocate a 2 MiB frame")?;
            match self.map_2m(HEAP_VIRT + index * HUGE_PAGE, frame.as_u64(), KERNEL_RW)? {
                HugeMap::Mapped => {}
                HugeMap::AlreadyPresent | HugeMap::Split => {
                    return Err("heap virtual address is already mapped");
                }
            }
        }
        Ok(())
    }

    fn map_user(&mut self, virt: u64, phys: u64, flags: u64) -> Result<(), &'static str> {
        if virt & 0xFFF != 0 || phys & 0xFFF != 0 {
            return Err("user map is misaligned");
        }
        let va = VirtualAddress::<PagingLevel4>::try_new(virt).map_err(|_| "non-canonical virtual address")?;
        let pdpt = self.descend_user(self.pml4, va.pml4_index())?;
        let pd = self.descend_user(pdpt, va.pdpt_index())?;
        let pt = self.descend_user(pd, va.pd_index())?;
        unsafe {
            ((self.hhdm + pt) as *mut u64)
                .add(va.pt_index())
                .write_volatile((phys & PHYS_MASK) | self.flags(flags));
        }
        Ok(())
    }

    fn descend_user(&mut self, table: u64, index: usize) -> Result<u64, &'static str> {
        let slot = unsafe { ((self.hhdm + table) as *mut u64).add(index) };
        let entry = unsafe { slot.read_volatile() };
        if entry & PRESENT != 0 {
            if entry & HUGE != 0 {
                return Err("user page walk hit a huge page");
            }
            if entry & USER == 0 {
                unsafe { slot.write_volatile(entry | USER) };
            }
            return Ok(entry & PHYS_MASK);
        }
        let frame = self
            .alloc
            .allocate(PageSize::Size4KiB)
            .map_err(|_| "out of frames for a user page table")?;
        let phys = frame.as_u64();
        unsafe {
            core::ptr::write_bytes((self.hhdm + phys) as *mut u8, 0, PAGE_SIZE as usize);
            slot.write_volatile(phys | PRESENT | WRITABLE | USER);
        }
        Ok(phys)
    }

    fn map_4k(&mut self, virt: u64, phys: u64, flags: u64) -> Result<(), &'static str> {
        if virt & 0xFFF != 0 || phys & 0xFFF != 0 {
            return Err("4 KiB map is misaligned");
        }
        let va = VirtualAddress::<PagingLevel4>::try_new(virt).map_err(|_| "non-canonical virtual address")?;
        let pdpt = self.descend(self.pml4, va.pml4_index())?;
        let pd = self.descend(pdpt, va.pdpt_index())?;
        let pt = self.descend(pd, va.pd_index())?;
        unsafe {
            ((self.hhdm + pt) as *mut u64)
                .add(va.pt_index())
                .write_volatile((phys & PHYS_MASK) | self.flags(flags));
        }
        Ok(())
    }

    fn map_2m(&mut self, virt: u64, phys: u64, flags: u64) -> Result<HugeMap, &'static str> {
        if virt & (HUGE_PAGE - 1) != 0 || phys & (HUGE_PAGE - 1) != 0 {
            return Err("2 MiB map is misaligned");
        }
        let va = VirtualAddress::<PagingLevel4>::try_new(virt).map_err(|_| "non-canonical virtual address")?;
        let pdpt = self.descend(self.pml4, va.pml4_index())?;
        let pd = self.descend(pdpt, va.pdpt_index())?;
        let slot = unsafe { ((self.hhdm + pd) as *mut u64).add(va.pd_index()) };
        let existing = unsafe { slot.read_volatile() };
        if existing & PRESENT != 0 {
            if existing & HUGE != 0 && (existing & PHYS_MASK) == (phys & PHYS_MASK) {
                return Ok(HugeMap::AlreadyPresent);
            }
            if existing & HUGE != 0 {
                return Err("2 MiB map conflicts with an existing huge page");
            }
            return Ok(HugeMap::Split);
        }
        unsafe {
            slot.write_volatile((phys & PHYS_MASK) | self.flags(flags) | HUGE);
        }
        Ok(HugeMap::Mapped)
    }

    fn descend(&mut self, table: u64, index: usize) -> Result<u64, &'static str> {
        let slot = unsafe { ((self.hhdm + table) as *mut u64).add(index) };
        let entry = unsafe { slot.read_volatile() };
        if entry & PRESENT != 0 {
            if entry & HUGE != 0 {
                return Err("page walk hit a huge page");
            }
            return Ok(entry & PHYS_MASK);
        }
        let frame = self
            .alloc
            .allocate(PageSize::Size4KiB)
            .map_err(|_| "out of frames for a page table")?;
        let phys = frame.as_u64();
        unsafe {
            core::ptr::write_bytes((self.hhdm + phys) as *mut u8, 0, PAGE_SIZE as usize);
            slot.write_volatile(phys | PRESENT | WRITABLE);
        }
        Ok(phys)
    }
}

enum HugeMap {
    Mapped,
    AlreadyPresent,
    Split,
}

fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

fn align_up(value: u64, align: u64) -> u64 {
    value.wrapping_add(align - 1) & !(align - 1)
}
