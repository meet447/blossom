//! Map an ET_EXEC ELF into a private address space.

use crate::mm::layout::{USER_RING, USER_STACK};
use crate::mm::{self, UserPerm};
use meuxe_abi::RingPage;
use meuxe_elf::{ElfError, ElfImage};

pub struct LoadedElf {
    pub entry: u64,
    pub cr3: u64,
    pub ring_phys: u64,
}

pub fn load(image: &[u8]) -> Result<LoadedElf, &'static str> {
    let elf = ElfImage::parse(image).map_err(ElfError::as_str)?;
    let cr3 = mm::new_address_space()?;
    for segment in elf.segments() {
        let segment = segment.map_err(ElfError::as_str)?;
        if segment.writable && segment.executable {
            return Err("elf segment is writable and executable");
        }
        if overlaps_reserved(segment.virt, segment.memsz) {
            return Err("elf segment overlaps a reserved user page");
        }
        let perm = if segment.executable {
            UserPerm::Rx
        } else if segment.writable {
            UserPerm::Rw
        } else {
            UserPerm::Ro
        };
        map_segment(cr3, image, segment.virt, segment.offset, segment.filesz, segment.memsz, perm)?;
    }
    let stack = mm::alloc_frame_zeroed()?;
    mm::map_user_in(cr3, USER_STACK, stack, UserPerm::Rw)?;
    let ring_phys = mm::alloc_frame_zeroed()?;
    unsafe {
        ((mm::hhdm() + ring_phys) as *mut RingPage).write(RingPage::new());
    }
    mm::map_user_in(cr3, USER_RING, ring_phys, UserPerm::Rw)?;
    Ok(LoadedElf {
        entry: elf.entry(),
        cr3,
        ring_phys,
    })
}

fn map_segment(
    cr3: u64,
    image: &[u8],
    virt: u64,
    offset: u64,
    filesz: u64,
    memsz: u64,
    perm: UserPerm,
) -> Result<(), &'static str> {
    let start = virt & !0xFFF;
    let end = virt
        .checked_add(memsz)
        .ok_or("elf segment overflows")?
        .wrapping_add(0xFFF)
        & !0xFFF;
    let mut page = start;
    while page < end {
        if mm::leaf_user(cr3, page).is_some() {
            return Err("elf segments share a page");
        }
        let frame = mm::alloc_frame_zeroed()?;
        let page_end = page + 4096;
        let data_lo = virt;
        let data_hi = virt + filesz;
        let lo = page.max(data_lo);
        let hi = page_end.min(data_hi);
        if lo < hi {
            let dst = (mm::hhdm() + frame + (lo - page)) as *mut u8;
            let src_off = (offset + (lo - virt)) as usize;
            let len = (hi - lo) as usize;
            let src = image
                .get(src_off..src_off + len)
                .ok_or("elf segment ran past the file")?;
            unsafe {
                core::ptr::copy_nonoverlapping(src.as_ptr(), dst, len);
            }
        }
        mm::map_user_in(cr3, page, frame, perm)?;
        page += 4096;
    }
    Ok(())
}

fn overlaps_reserved(virt: u64, len: u64) -> bool {
    let end = virt.saturating_add(len);
    let reserved = [
        USER_STACK,
        USER_RING,
        meuxe_abi::USER_INFO,
        meuxe_abi::USER_MMIO,
        meuxe_abi::USER_QUEUE,
        meuxe_abi::USER_SHARE,
        meuxe_abi::USER_FRONT,
        meuxe_abi::USER_BACK,
        meuxe_abi::USER_STATUS,
        meuxe_abi::USER_KBD_QUEUE,
        meuxe_abi::USER_KBD_EVENT,
        meuxe_abi::USER_KBD_READY,
        meuxe_abi::USER_FS,
        meuxe_abi::USER_FS_FILES,
        meuxe_abi::USER_PICK,
        meuxe_abi::USER_CALC_PICK,
    ];
    if reserved.iter().any(|page| virt < page + 0x1000 && *page < end) {
        return true;
    }
    let fb = meuxe_abi::USER_FB;
    if virt < fb + 8 * 1024 * 1024 && fb < end {
        return true;
    }
    let term = meuxe_abi::USER_TERM;
    if virt < term + 1024 * 1024 && term < end {
        return true;
    }
    let files = meuxe_abi::USER_FILES;
    if virt < files + 1024 * 1024 && files < end {
        return true;
    }
    let calc = meuxe_abi::USER_CALC;
    virt < calc + 1024 * 1024 && calc < end
}
