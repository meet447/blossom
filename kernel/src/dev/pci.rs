//! PCI configuration space, used to find virtio MMIO windows.
//!
//! MSI-X is enabled for virtio-blk (vector 33), the tablet (vector 34), and
//! the keyboard (vector 35). Every IOAPIC line stays masked.

use crate::arch::x86_64::cpu;
use crate::mm;
use crate::mm::layout::{MSI_STRIDE, MSI_VIRT};

const ADDRESS: u16 = 0xCF8;
const DATA: u16 = 0xCFC;
const MSI_ADDR: u32 = 0xFEE0_0000;

#[derive(Clone, Copy)]
enum MsixDev {
    Blk,
    Tablet,
    Keyboard,
}

impl MsixDev {
    fn vector(self) -> u32 {
        match self {
            Self::Blk => 33,
            Self::Tablet => 34,
            Self::Keyboard => 35,
        }
    }

    fn window(self) -> u64 {
        let slot = match self {
            Self::Blk => 0,
            Self::Tablet => 1,
            Self::Keyboard => 2,
        };
        MSI_VIRT + slot * MSI_STRIDE
    }

    fn label(self) -> &'static str {
        match self {
            Self::Blk => "virtio-blk",
            Self::Tablet => "virtio-tablet",
            Self::Keyboard => "virtio-keyboard",
        }
    }

    fn err_msix(self) -> &'static str {
        match self {
            Self::Blk => "virtio-blk has no msi-x",
            Self::Tablet => "virtio-tablet has no msi-x",
            Self::Keyboard => "virtio-keyboard has no msi-x",
        }
    }

    fn err_isr(self) -> &'static str {
        match self {
            Self::Blk => "virtio-blk has no isr",
            Self::Tablet => "virtio-tablet has no isr",
            Self::Keyboard => "virtio-keyboard has no isr",
        }
    }
}

pub struct VirtioDev {
    pub common: u64,
    pub common_len: u32,
    pub notify: u64,
    pub notify_len: u32,
    pub notify_mul: u32,
    pub device: u64,
    pub device_len: u32,
    pub isr: u64,
    pub isr_len: u32,
}

pub fn find_virtio_blk() -> Result<VirtioDev, &'static str> {
    find_virtio(&[0x1001, 0x1042], "virtio-blk device is missing")
}

pub fn find_virtio_tablet() -> Result<VirtioDev, &'static str> {
    find_virtio_input(0, "virtio-tablet device is missing")
}

pub fn find_virtio_keyboard() -> Result<VirtioDev, &'static str> {
    find_virtio_input(1, "virtio-keyboard device is missing")
}

fn find_virtio_input(which: usize, missing: &'static str) -> Result<VirtioDev, &'static str> {
    let mut seen = 0usize;
    for bus in 0..8u8 {
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let vendor = cfg_read(bus, dev, func, 0);
                if vendor == 0xFFFF_FFFF || (vendor & 0xFFFF) == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                let vid = (vendor & 0xFFFF) as u16;
                let did = (vendor >> 16) as u16;
                if vid == 0x1AF4 && did == 0x1052 {
                    if seen == which {
                        let device = parse_virtio(bus, dev, func)?;
                        let kind = if which == 0 {
                            MsixDev::Tablet
                        } else {
                            MsixDev::Keyboard
                        };
                        enable_msix(bus, dev, func, kind)?;
                        return Ok(device);
                    }
                    seen += 1;
                }
                if func == 0 {
                    let header = cfg_read(bus, dev, func, 0x0C);
                    if (header >> 16) & 0x80 == 0 {
                        break;
                    }
                }
            }
        }
    }
    Err(missing)
}

fn find_virtio(ids: &[u16], missing: &'static str) -> Result<VirtioDev, &'static str> {
    for bus in 0..8u8 {
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let vendor = cfg_read(bus, dev, func, 0);
                if vendor == 0xFFFF_FFFF || (vendor & 0xFFFF) == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                let vid = (vendor & 0xFFFF) as u16;
                let did = (vendor >> 16) as u16;
                if vid == 0x1AF4 && ids.contains(&did) {
                    let device = parse_virtio(bus, dev, func)?;
                    enable_msix(bus, dev, func, MsixDev::Blk)?;
                    return Ok(device);
                }
                if func == 0 {
                    let header = cfg_read(bus, dev, func, 0x0C);
                    if (header >> 16) & 0x80 == 0 {
                        break;
                    }
                }
            }
        }
    }
    Err(missing)
}

fn parse_virtio(bus: u8, dev: u8, func: u8) -> Result<VirtioDev, &'static str> {
    let status = cfg_read(bus, dev, func, 0x04) >> 16;
    if status & (1 << 4) == 0 {
        return Err("virtio-blk has no pci capabilities");
    }
    let mut cap = (cfg_read(bus, dev, func, 0x34) & 0xFF) as u8;
    let mut common = None;
    let mut notify = None;
    let mut device = None;
    let mut isr = None;
    let mut notify_mul = 0u32;
    for _ in 0..32 {
        if cap < 0x40 || cap & 3 != 0 {
            break;
        }
        let w0 = cfg_read(bus, dev, func, cap);
        let next = ((w0 >> 8) & 0xFF) as u8;
        let id = (w0 & 0xFF) as u8;
        if id == 0x09 {
            let w1 = cfg_read(bus, dev, func, cap + 4);
            let offset = cfg_read(bus, dev, func, cap + 8);
            let length = cfg_read(bus, dev, func, cap + 12);
            let bar = (w1 & 0xFF) as u8;
            let kind = ((w0 >> 24) & 0xFF) as u8;
            let base = bar_base(bus, dev, func, bar)?;
            let phys = base + offset as u64;
            match kind {
                1 => common = Some((phys, length)),
                2 => {
                    notify = Some((phys, length));
                    notify_mul = cfg_read(bus, dev, func, cap + 16);
                }
                3 => isr = Some((phys, length)),
                4 => device = Some((phys, length)),
                _ => {}
            }
        }
        if next == 0 || next == cap {
            break;
        }
        cap = next;
    }
    let (common, common_len) = common.ok_or("virtio-blk has no common config")?;
    let (notify, notify_len) = notify.ok_or("virtio-blk has no notify config")?;
    let (device, device_len) = device.unwrap_or((0, 0));
    let (isr, isr_len) = isr.unwrap_or((0, 0));
    enable_mem_and_bus_master(bus, dev, func);
    Ok(VirtioDev {
        common,
        common_len,
        notify,
        notify_len,
        notify_mul,
        device,
        device_len,
        isr,
        isr_len,
    })
}

fn enable_msix(bus: u8, dev: u8, func: u8, kind: MsixDev) -> Result<(), &'static str> {
    let mut cap = (cfg_read(bus, dev, func, 0x34) & 0xFF) as u8;
    let mut msix = None;
    let mut isr_phys = 0u64;
    for _ in 0..48 {
        if cap < 0x40 || cap & 3 != 0 {
            break;
        }
        let w0 = cfg_read(bus, dev, func, cap);
        let next = ((w0 >> 8) & 0xFF) as u8;
        let id = (w0 & 0xFF) as u8;
        if id == 0x11 {
            msix = Some(cap);
        }
        if id == 0x09 && ((w0 >> 24) & 0xFF) == 3 {
            let w1 = cfg_read(bus, dev, func, cap + 4);
            let offset = cfg_read(bus, dev, func, cap + 8);
            let bar = (w1 & 0xFF) as u8;
            isr_phys = bar_base(bus, dev, func, bar)? + offset as u64;
        }
        if next == 0 || next == cap {
            break;
        }
        cap = next;
    }
    let cap = msix.ok_or(kind.err_msix())?;
    if isr_phys == 0 {
        return Err(kind.err_isr());
    }
    let table = cfg_read(bus, dev, func, cap + 4);
    let bir = (table & 7) as u8;
    let offset = table & !7;
    let table_phys = bar_base(bus, dev, func, bir)? + offset as u64;
    let window = kind.window();
    let page = table_phys & !0xFFF;
    mm::map_kernel_mmio(window, page)?;
    if (table_phys & 0xFFF) + 16 > 0x1000 {
        mm::map_kernel_mmio(window + 0x1000, page + 0x1000)?;
    }
    let entry = window + (table_phys & 0xFFF);
    let dest = crate::arch::x86_64::apic::id(crate::mm::layout::LAPIC_VIRT) & 0xFF;
    let msi_addr = MSI_ADDR | (dest << 12);
    let vector = kind.vector();
    unsafe {
        (entry as *mut u32).write_volatile(msi_addr);
        ((entry + 4) as *mut u32).write_volatile(0);
        ((entry + 8) as *mut u32).write_volatile(vector);
        ((entry + 12) as *mut u32).write_volatile(1);
    }
    let control = cfg_read(bus, dev, func, cap) >> 16;
    let control = (control | (1 << 15)) & !(1 << 14);
    cfg_write_word(bus, dev, func, cap + 2, control as u16);
    unsafe {
        ((entry + 12) as *mut u32).write_volatile(0);
    }
    let isr_page = isr_phys & !0xFFF;
    mm::map_kernel_mmio(window + 0x2000, isr_page)?;
    crate::dev::irq::arm(vector as u8, window + 0x2000 + (isr_phys & 0xFFF));
    let label = kind.label();
    crate::kprintln!("meuxe: {label} msix vector={vector}");
    Ok(())
}

fn enable_mem_and_bus_master(bus: u8, dev: u8, func: u8) {
    let addr = address(bus, dev, func, 0x04);
    cpu::outl(ADDRESS, addr);
    let command = cpu::inw(DATA);
    cpu::outw(DATA, command | 0x6);
}

fn bar_base(bus: u8, dev: u8, func: u8, index: u8) -> Result<u64, &'static str> {
    if index > 5 {
        return Err("virtio-blk bar index is invalid");
    }
    let offset = 0x10 + index * 4;
    let raw = cfg_read(bus, dev, func, offset);
    if raw & 1 != 0 {
        return Err("virtio-blk bar is io, not memory");
    }
    let kind = (raw >> 1) & 3;
    if kind == 2 {
        let high = cfg_read(bus, dev, func, offset + 4);
        Ok(((high as u64) << 32) | (raw as u64 & 0xFFFF_FFF0))
    } else {
        Ok(raw as u64 & 0xFFFF_FFF0)
    }
}

fn cfg_read(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    cpu::outl(ADDRESS, address(bus, dev, func, offset));
    cpu::inl(DATA)
}

fn cfg_write_word(bus: u8, dev: u8, func: u8, offset: u8, value: u16) {
    cpu::outl(ADDRESS, address(bus, dev, func, offset));
    cpu::outw(DATA + (offset & 2) as u16, value);
}

fn address(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000 | ((bus as u32) << 16) | ((dev as u32) << 11) | ((func as u32) << 8) | (offset as u32 & 0xFC)
}
