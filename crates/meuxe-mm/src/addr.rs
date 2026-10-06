//! Physical addresses and 4-level canonical virtual addresses.

use core::fmt;
use core::marker::PhantomData;

/// Marker for x86_64 4-level paging (48-bit canonical virtual addresses).
#[derive(Clone, Copy, Debug, Default)]
pub struct PagingLevel4;

/// A physical address. Alignment is checked by the operation that needs it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct PhysicalAddress(u64);

impl PhysicalAddress {
    pub const fn new(addr: u64) -> Self {
        Self(addr)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn is_aligned(self, align: u64) -> bool {
        align != 0 && align.is_power_of_two() && self.0 & (align - 1) == 0
    }

    pub const fn align_down(self, align: u64) -> Self {
        Self(self.0 & !(align - 1))
    }

    pub const fn align_up(self, align: u64) -> Self {
        Self(self.0.wrapping_add(align - 1) & !(align - 1))
    }

    pub const fn checked_add(self, bytes: u64) -> Option<Self> {
        match self.0.checked_add(bytes) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl fmt::Debug for PhysicalAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhysicalAddress({:#x})", self.0)
    }
}

/// Canonical virtual address for a paging level.
///
/// Only [`PagingLevel4`] is implemented. Bits 48..=63 must copy bit 47.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct VirtualAddress<Level> {
    addr: u64,
    _level: PhantomData<Level>,
}

impl<Level> fmt::Debug for VirtualAddress<Level> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VirtualAddress({:#x})", self.addr)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressError {
    NonCanonical,
    OutOfRange,
}

impl VirtualAddress<PagingLevel4> {
    pub const fn is_canonical(addr: u64) -> bool {
        let top = addr >> 47;
        top == 0 || top == 0x1_FFFF
    }

    pub const fn try_new(addr: u64) -> Result<Self, AddressError> {
        if Self::is_canonical(addr) {
            Ok(Self {
                addr,
                _level: PhantomData,
            })
        } else {
            Err(AddressError::NonCanonical)
        }
    }

    /// Sign-extend bit 47 so the result is canonical.
    pub const fn new_truncate(addr: u64) -> Self {
        let sign = (addr >> 47) & 1;
        let low = addr & 0x0000_7FFF_FFFF_FFFF;
        let addr = if sign == 0 {
            low
        } else {
            low | 0xFFFF_8000_0000_0000
        };
        Self {
            addr,
            _level: PhantomData,
        }
    }

    pub const fn as_u64(self) -> u64 {
        self.addr
    }

    pub const fn pml4_index(self) -> usize {
        ((self.addr >> 39) & 0x1ff) as usize
    }

    pub const fn pdpt_index(self) -> usize {
        ((self.addr >> 30) & 0x1ff) as usize
    }

    pub const fn pd_index(self) -> usize {
        ((self.addr >> 21) & 0x1ff) as usize
    }

    pub const fn pt_index(self) -> usize {
        ((self.addr >> 12) & 0x1ff) as usize
    }

    pub const fn page_offset(self) -> u64 {
        self.addr & 0xfff
    }

    pub const fn from_indices(
        pml4: u64,
        pdpt: u64,
        pd: u64,
        pt: u64,
        offset: u64,
    ) -> Result<Self, AddressError> {
        if pml4 > 511 || pdpt > 511 || pd > 511 || pt > 511 || offset > 0xfff {
            return Err(AddressError::OutOfRange);
        }
        let mut addr = (pml4 << 39) | (pdpt << 30) | (pd << 21) | (pt << 12) | offset;
        if pml4 & 0x100 != 0 {
            addr |= 0xFFFF_0000_0000_0000;
        }
        Self::try_new(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_canonical() {
        assert!(VirtualAddress::<PagingLevel4>::try_new(0x0000_8000_0000_0000).is_err());
        assert!(VirtualAddress::<PagingLevel4>::try_new(0).is_ok());
        assert!(VirtualAddress::<PagingLevel4>::try_new(0xFFFF_8000_0000_0000).is_ok());
        assert!(VirtualAddress::<PagingLevel4>::try_new(0x0000_7FFF_FFFF_FFFF).is_ok());
    }

    #[test]
    fn kernel_link_address_indices() {
        let va = VirtualAddress::<PagingLevel4>::try_new(0xFFFF_FFFF_8000_0000).unwrap();
        assert_eq!(va.pml4_index(), 511);
        assert_eq!(va.pdpt_index(), 510);
        assert_eq!(va.pd_index(), 0);
        assert_eq!(va.pt_index(), 0);
        assert_eq!(va.page_offset(), 0);
        let rebuilt = VirtualAddress::<PagingLevel4>::from_indices(511, 510, 0, 0, 0).unwrap();
        assert_eq!(rebuilt.as_u64(), 0xFFFF_FFFF_8000_0000);
    }

    #[test]
    fn truncate_sign_extends() {
        let high = VirtualAddress::<PagingLevel4>::new_truncate(0x0000_FFFF_8000_1234);
        assert_eq!(high.as_u64(), 0xFFFF_FFFF_8000_1234);
        let low = VirtualAddress::<PagingLevel4>::new_truncate(0x0000_0000_0000_1234);
        assert_eq!(low.as_u64(), 0x1234);
    }

    #[test]
    fn physical_alignment() {
        let addr = PhysicalAddress::new(0x1234);
        assert_eq!(addr.align_down(0x1000).as_u64(), 0x1000);
        assert_eq!(addr.align_up(0x1000).as_u64(), 0x2000);
        assert!(PhysicalAddress::new(0x200000).is_aligned(0x200000));
    }
}
