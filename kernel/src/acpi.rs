//! Enough ACPI to find the MADT and the PM timer. Tables are read through
//! Limine's higher-half map, before Meuxe installs its own page tables.

#[derive(Clone, Copy)]
pub struct IoApic {
    pub phys: u64,
    pub gsi_base: u32,
}

pub struct AcpiInfo {
    pub lapic_phys: u64,
    pub ioapics: [IoApic; 8],
    pub ioapic_count: usize,
    pub cpu_count: usize,
    pub pm_port: u16,
    pub pm_wide: bool,
}

pub fn parse(rsdp_virt: u64, hhdm: u64, lapic_fallback: u64) -> AcpiInfo {
    let mut info = AcpiInfo {
        lapic_phys: lapic_fallback,
        ioapics: [IoApic {
            phys: 0,
            gsi_base: 0,
        }; 8],
        ioapic_count: 0,
        cpu_count: 0,
        pm_port: 0,
        pm_wide: false,
    };
    if rsdp_virt == 0 {
        return info;
    }
    let rsdp = unsafe { core::slice::from_raw_parts(rsdp_virt as *const u8, 36) };
    if &rsdp[0..8] != b"RSD PTR " {
        return info;
    }
    if checksum(&rsdp[..20]) != 0 {
        return info;
    }
    let revision = rsdp[15];
    let table_phys = if revision >= 2 && checksum(rsdp) == 0 {
        let xsdt = read_u64(rsdp, 24);
        if xsdt != 0 {
            xsdt
        } else {
            read_u32(rsdp, 16) as u64
        }
    } else {
        read_u32(rsdp, 16) as u64
    };
    if table_phys == 0 {
        return info;
    }
    let Some(root) = map_table(hhdm, table_phys) else {
        return info;
    };
    let entry_size = if &root[0..4] == b"XSDT" { 8 } else { 4 };
    if root.len() < 36 {
        return info;
    }
    let mut offset = 36;
    while offset + entry_size <= root.len() {
        let phys = if entry_size == 8 {
            read_u64(root, offset)
        } else {
            read_u32(root, offset) as u64
        };
        offset += entry_size;
        let Some(table) = map_table(hhdm, phys) else {
            continue;
        };
        if table.len() < 4 {
            continue;
        }
        match &table[0..4] {
            b"APIC" => parse_madt(table, &mut info),
            b"FACP" => parse_fadt(table, &mut info),
            _ => {}
        }
    }
    info
}

fn parse_madt(table: &[u8], info: &mut AcpiInfo) {
    if table.len() < 44 {
        return;
    }
    let lapic = read_u32(table, 36) as u64;
    if lapic != 0 {
        info.lapic_phys = lapic;
    }
    let mut offset = 44;
    while offset + 2 <= table.len() {
        let kind = table[offset];
        let len = table[offset + 1] as usize;
        if len < 2 || offset + len > table.len() {
            break;
        }
        match kind {
            0 if len >= 8 => {
                let flags = read_u32(table, offset + 4);
                if flags & 1 != 0 {
                    info.cpu_count += 1;
                }
            }
            1 if len >= 12 => {
                if info.ioapic_count < info.ioapics.len() {
                    let index = info.ioapic_count;
                    info.ioapics[index] = IoApic {
                        phys: read_u32(table, offset + 4) as u64,
                        gsi_base: read_u32(table, offset + 8),
                    };
                    info.ioapic_count += 1;
                }
            }
            _ => {}
        }
        offset += len;
    }
}

fn parse_fadt(table: &[u8], info: &mut AcpiInfo) {
    if table.len() < 92 {
        return;
    }
    let mut port = read_u32(table, 76);
    let mut wide = table[91] >= 4;
    if table.len() >= 116 {
        let flags = read_u32(table, 112);
        if flags & (1 << 8) != 0 {
            wide = true;
        }
    }
    if table.len() >= 220 {
        let space = table[208];
        let bit_width = table[209];
        let addr = read_u64(table, 212);
        if space == 1 && addr != 0 && addr <= u16::MAX as u64 {
            port = addr as u32;
            if bit_width >= 32 {
                wide = true;
            }
        }
    }
    if port != 0 && port <= u16::MAX as u32 {
        info.pm_port = port as u16;
        info.pm_wide = wide;
    }
}

fn map_table(hhdm: u64, phys: u64) -> Option<&'static [u8]> {
    if phys == 0 {
        return None;
    }
    let header = unsafe { core::slice::from_raw_parts((hhdm + phys) as *const u8, 8) };
    let length = read_u32(header, 4) as usize;
    if length < 36 || length > 1024 * 1024 {
        return None;
    }
    let table = unsafe { core::slice::from_raw_parts((hhdm + phys) as *const u8, length) };
    if checksum(table) != 0 {
        return None;
    }
    Some(table)
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
