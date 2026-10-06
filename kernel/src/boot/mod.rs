//! Collect Limine's responses into owned values before page tables are replaced.

mod limine;

use crate::acpi::{self, IoApic};
use crate::arch::x86_64::cpu;
use crate::mm::layout::{self, HEAP_SIZE, HEAP_VIRT, IOAPIC_STRIDE, IOAPIC_VIRT, LAPIC_VIRT};

pub use limine::{
    MEMMAP_ACPI_NVS, MEMMAP_ACPI_RECLAIMABLE, MEMMAP_BAD_MEMORY, MEMMAP_BOOTLOADER_RECLAIMABLE,
    MEMMAP_EXECUTABLE_AND_MODULES, MEMMAP_FRAMEBUFFER, MEMMAP_RESERVED, MEMMAP_RESERVED_MAPPED,
    MEMMAP_USABLE, MpInfo,
};

const MAX_REGIONS: usize = 128;

#[derive(Clone, Copy)]
pub struct MemRegion {
    pub base: u64,
    pub length: u64,
    pub kind: u64,
}

#[derive(Clone, Copy)]
pub struct FbInfo {
    pub virt: u64,
    pub phys: u64,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

#[derive(Clone, Copy)]
pub struct MappedIoApic {
    pub phys: u64,
    pub virt: u64,
    pub gsi_base: u32,
}

pub struct BootInfo {
    pub revision: u64,
    pub firmware: u64,
    pub name: [u8; 64],
    pub name_len: usize,
    pub version: [u8; 64],
    pub version_len: usize,
    pub hhdm: u64,
    pub kernel_phys: u64,
    pub kernel_virt: u64,
    pub regions: [MemRegion; MAX_REGIONS],
    pub region_count: usize,
    pub fb: Option<FbInfo>,
    pub tsc_hz: u64,
    pub pm_port: u16,
    pub pm_wide: bool,
    pub lapic_phys: u64,
    pub lapic_virt: u64,
    pub ioapics: [MappedIoApic; 8],
    pub ioapic_count: usize,
    pub cpu_count: usize,
    pub heap_virt: u64,
    pub heap_size: u64,
    pub mp_cpus: [u64; 8],
    pub mp_count: usize,
    pub bsp_lapic_id: u32,
}

impl BootInfo {
    pub fn regions(&self) -> &[MemRegion] {
        &self.regions[..self.region_count]
    }

    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
    }

    pub fn version(&self) -> &str {
        core::str::from_utf8(&self.version[..self.version_len]).unwrap_or("?")
    }
}

pub fn firmware_name(kind: u64) -> &'static str {
    match kind {
        0 => "bios",
        1 => "efi32",
        2 => "efi64",
        3 => "sbi",
        _ => "unknown",
    }
}

pub fn collect() -> Result<BootInfo, &'static str> {
    // Keep the request objects live. The bootloader finds them by magic, not
    // by a symbol, but a direct reference stops the linker discarding them.
    let _anchors = (
        core::ptr::addr_of!(limine::START_MARKER),
        core::ptr::addr_of!(limine::BASE_REVISION),
        core::ptr::addr_of!(limine::END_MARKER),
        core::ptr::addr_of!(limine::MEMMAP),
        core::ptr::addr_of!(limine::HHDM),
        core::ptr::addr_of!(limine::FRAMEBUFFER),
        core::ptr::addr_of!(limine::PAGING_MODE),
        core::ptr::addr_of!(limine::STACK),
        core::ptr::addr_of!(limine::MP),
    );
    let _ = _anchors;

    if !limine::BASE_REVISION.is_supported() {
        return Err("limine base revision was not accepted");
    }
    let revision = limine::BASE_REVISION.loaded_revision().unwrap_or(0);
    if let Some(mode) = limine::PAGING_MODE.mode() {
        if mode != 0 {
            return Err("limine did not enter on 4-level paging");
        }
    }

    let hhdm = limine::HHDM.get().ok_or("missing hhdm")?.offset;
    let executable = limine::EXECUTABLE_ADDRESS
        .get()
        .ok_or("missing executable address")?;
    let memmap = limine::MEMMAP.get().ok_or("missing memory map")?;
    let entries = memmap.entries();
    if entries.len() > MAX_REGIONS {
        return Err("memory map has more entries than the boot map stores");
    }

    let mut regions = [MemRegion {
        base: 0,
        length: 0,
        kind: 0,
    }; MAX_REGIONS];
    for (index, entry) in entries.iter().enumerate() {
        regions[index] = MemRegion {
            base: entry.base,
            length: entry.length,
            kind: entry.kind,
        };
    }

    let fb = limine::FRAMEBUFFER.get().and_then(|list| {
        let fb = list.first()?;
        if (fb.address as u64) < hhdm || fb.width == 0 || fb.height == 0 {
            return None;
        }
        Some(FbInfo {
            virt: fb.address as u64,
            phys: fb.address as u64 - hhdm,
            width: fb.width as u32,
            height: fb.height as u32,
            pitch: fb.pitch as u32,
            bpp: fb.bpp,
            red_shift: fb.red_mask_shift,
            green_shift: fb.green_mask_shift,
            blue_shift: fb.blue_mask_shift,
        })
    });

    let lapic_phys = cpu::lapic_physical_base();
    let rsdp = limine::RSDP
        .get()
        .map(|rsdp| rsdp.address as u64)
        .unwrap_or(0);
    let acpi = acpi::parse(rsdp, hhdm, lapic_phys);
    let mut ioapics = [MappedIoApic {
        phys: 0,
        virt: 0,
        gsi_base: 0,
    }; 8];
    for index in 0..acpi.ioapic_count {
        ioapics[index] = map_ioapic(index, acpi.ioapics[index]);
    }

    let mut mp_cpus = [0u64; 8];
    let mut mp_count = 0usize;
    let mut bsp_lapic_id = 0u32;
    if let Some(mp) = limine::MP.get() {
        bsp_lapic_id = mp.bsp_lapic_id;
        mp_count = (mp.cpu_count as usize).min(mp_cpus.len());
        for index in 0..mp_count {
            let cpu = unsafe { mp.cpus.add(index).read_volatile() };
            mp_cpus[index] = cpu as u64;
        }
    }

    let mut name = [0u8; 64];
    let mut version = [0u8; 64];
    let (name_len, version_len) = if let Some(info) = limine::BOOTLOADER_INFO.get() {
        (
            copy_cstr(info.name, &mut name),
            copy_cstr(info.version, &mut version),
        )
    } else {
        (0, 0)
    };

    Ok(BootInfo {
        revision,
        firmware: limine::FIRMWARE
            .get()
            .map(|fw| fw.firmware_type)
            .unwrap_or(u64::MAX),
        name,
        name_len,
        version,
        version_len,
        hhdm,
        kernel_phys: executable.physical_base,
        kernel_virt: executable.virtual_base,
        regions,
        region_count: entries.len(),
        fb,
        tsc_hz: limine::TSC.get().map(|tsc| tsc.frequency).unwrap_or(0),
        pm_port: acpi.pm_port,
        pm_wide: acpi.pm_wide,
        lapic_phys: acpi.lapic_phys,
        lapic_virt: LAPIC_VIRT,
        ioapics,
        ioapic_count: acpi.ioapic_count,
        cpu_count: acpi.cpu_count,
        heap_virt: HEAP_VIRT,
        heap_size: HEAP_SIZE,
        mp_cpus,
        mp_count,
        bsp_lapic_id,
    })
}

fn map_ioapic(index: usize, apic: IoApic) -> MappedIoApic {
    MappedIoApic {
        phys: apic.phys,
        virt: IOAPIC_VIRT + index as u64 * IOAPIC_STRIDE,
        gsi_base: apic.gsi_base,
    }
}

fn copy_cstr(ptr: *const u8, dest: &mut [u8]) -> usize {
    if ptr.is_null() {
        return 0;
    }
    let mut len = 0;
    while len + 1 < dest.len() {
        let byte = unsafe { ptr.add(len).read_volatile() };
        if byte == 0 {
            break;
        }
        dest[len] = byte;
        len += 1;
    }
    len
}

pub fn kernel_image_bytes(boot: &BootInfo) -> u64 {
    layout::kernel_end().saturating_sub(boot.kernel_virt)
}
