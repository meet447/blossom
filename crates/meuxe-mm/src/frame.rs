//! Two-level physical frame allocator.
//!
//! Level 0 is one bit per 4 KiB frame. A 2 MiB allocation is 512 aligned
//! level-0 bits (eight `u64` words). A set bit means the frame is in use.
//! Fresh allocators start fully allocated; the caller frees usable ranges.

use crate::PhysicalAddress;

pub const PAGE_SIZE: u64 = 4096;
pub const HUGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;
const HUGE_FRAMES: usize = (HUGE_PAGE_SIZE / PAGE_SIZE) as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSize {
    Size4KiB,
    Size2MiB,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocError {
    NoMemory,
    Misaligned,
    OutOfRange,
    DoubleFree,
    BitmapTooSmall,
}

pub struct BitmapFrameAllocator<'a> {
    origin: u64,
    frame_count: usize,
    free_count: usize,
    l0: &'a mut [u64],
}

impl<'a> BitmapFrameAllocator<'a> {
    /// `origin` must be 2 MiB aligned. Every frame starts in use.
    pub fn new(
        origin: PhysicalAddress,
        frame_count: usize,
        l0: &'a mut [u64],
    ) -> Result<Self, AllocError> {
        if !origin.is_aligned(HUGE_PAGE_SIZE) {
            return Err(AllocError::Misaligned);
        }
        let words = frame_count.div_ceil(64);
        if l0.len() < words {
            return Err(AllocError::BitmapTooSmall);
        }
        for word in l0.iter_mut().take(words) {
            *word = u64::MAX;
        }
        Ok(Self {
            origin: origin.as_u64(),
            frame_count,
            free_count: 0,
            l0,
        })
    }

    pub fn frame_count(&self) -> usize {
        self.frame_count
    }

    pub fn free_frames(&self) -> usize {
        self.free_count
    }

    pub fn is_allocated(&self, addr: PhysicalAddress) -> bool {
        match self.index(addr.as_u64()) {
            Ok(idx) => self.test(idx),
            Err(_) => true,
        }
    }

    pub fn allocate(&mut self, size: PageSize) -> Result<PhysicalAddress, AllocError> {
        match size {
            PageSize::Size4KiB => self.allocate_4k(),
            PageSize::Size2MiB => self.allocate_2m(),
        }
    }

    pub fn free(&mut self, addr: PhysicalAddress, size: PageSize) -> Result<(), AllocError> {
        match size {
            PageSize::Size4KiB => self.free_4k(addr),
            PageSize::Size2MiB => self.free_2m(addr),
        }
    }

    /// Mark every fully covered frame in `[start, start + bytes)` as free.
    /// Partial edge pages are left untouched. Already-free frames stay free.
    pub fn mark_free(&mut self, start: PhysicalAddress, bytes: u64) -> Result<(), AllocError> {
        let Some(end) = start.as_u64().checked_add(bytes) else {
            return Err(AllocError::OutOfRange);
        };
        let first = align_up(start.as_u64(), PAGE_SIZE);
        let last = align_down(end, PAGE_SIZE);
        let mut addr = first;
        while addr < last {
            if let Ok(idx) = self.index(addr) {
                self.set_free(idx);
            }
            addr += PAGE_SIZE;
        }
        Ok(())
    }

    /// Mark every frame that intersects `[start, start + bytes)` as in use.
    pub fn mark_used(&mut self, start: PhysicalAddress, bytes: u64) -> Result<(), AllocError> {
        if bytes == 0 {
            return Ok(());
        }
        let Some(end) = start.as_u64().checked_add(bytes) else {
            return Err(AllocError::OutOfRange);
        };
        let first = align_down(start.as_u64(), PAGE_SIZE);
        let last = align_up(end, PAGE_SIZE);
        let mut addr = first;
        while addr < last {
            if let Ok(idx) = self.index(addr) {
                self.set_used(idx);
            }
            addr += PAGE_SIZE;
        }
        Ok(())
    }

    fn allocate_4k(&mut self) -> Result<PhysicalAddress, AllocError> {
        let words = self.frame_count.div_ceil(64);
        for word in 0..words {
            let bits = self.l0[word];
            if bits == u64::MAX {
                continue;
            }
            let bit = bits.trailing_ones() as usize;
            let idx = word * 64 + bit;
            if idx >= self.frame_count {
                break;
            }
            self.set_used(idx);
            return Ok(PhysicalAddress::new(self.origin + idx as u64 * PAGE_SIZE));
        }
        Err(AllocError::NoMemory)
    }

    fn allocate_2m(&mut self) -> Result<PhysicalAddress, AllocError> {
        let groups = self.frame_count / HUGE_FRAMES;
        for group in 0..groups {
            let word = group * (HUGE_FRAMES / 64);
            if (0..8).all(|offset| self.l0[word + offset] == 0) {
                for offset in 0..8 {
                    self.l0[word + offset] = u64::MAX;
                }
                self.free_count -= HUGE_FRAMES;
                let idx = group * HUGE_FRAMES;
                return Ok(PhysicalAddress::new(self.origin + idx as u64 * PAGE_SIZE));
            }
        }
        Err(AllocError::NoMemory)
    }

    fn free_4k(&mut self, addr: PhysicalAddress) -> Result<(), AllocError> {
        let idx = self.index(addr.as_u64())?;
        if !self.test(idx) {
            return Err(AllocError::DoubleFree);
        }
        self.set_free(idx);
        Ok(())
    }

    fn free_2m(&mut self, addr: PhysicalAddress) -> Result<(), AllocError> {
        if !addr.is_aligned(HUGE_PAGE_SIZE) {
            return Err(AllocError::Misaligned);
        }
        let idx = self.index(addr.as_u64())?;
        if idx % HUGE_FRAMES != 0 || idx + HUGE_FRAMES > self.frame_count {
            return Err(AllocError::OutOfRange);
        }
        let word = idx / 64;
        if (0..8).any(|offset| self.l0[word + offset] != u64::MAX) {
            return Err(AllocError::DoubleFree);
        }
        for offset in 0..8 {
            self.l0[word + offset] = 0;
        }
        self.free_count += HUGE_FRAMES;
        Ok(())
    }

    fn index(&self, phys: u64) -> Result<usize, AllocError> {
        if phys < self.origin || (phys - self.origin) % PAGE_SIZE != 0 {
            return Err(AllocError::Misaligned);
        }
        let idx = ((phys - self.origin) / PAGE_SIZE) as usize;
        if idx >= self.frame_count {
            return Err(AllocError::OutOfRange);
        }
        Ok(idx)
    }

    fn test(&self, idx: usize) -> bool {
        let (word, bit) = (idx / 64, idx % 64);
        self.l0[word] & (1 << bit) != 0
    }

    fn set_used(&mut self, idx: usize) {
        let (word, bit) = (idx / 64, idx % 64);
        let mask = 1u64 << bit;
        if self.l0[word] & mask == 0 {
            self.l0[word] |= mask;
            self.free_count -= 1;
        }
    }

    fn set_free(&mut self, idx: usize) {
        let (word, bit) = (idx / 64, idx % 64);
        let mask = 1u64 << bit;
        if self.l0[word] & mask != 0 {
            self.l0[word] &= !mask;
            self.free_count += 1;
        }
    }
}

const fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

const fn align_up(value: u64, align: u64) -> u64 {
    value.wrapping_add(align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocator(frames: usize) -> BitmapFrameAllocator<'static> {
        let words = frames.div_ceil(64);
        let storage = Box::leak(vec![0u64; words].into_boxed_slice());
        BitmapFrameAllocator::new(PhysicalAddress::new(0), frames, storage).unwrap()
    }

    #[test]
    fn four_k_alloc_and_double_free() {
        let mut alloc = allocator(1024);
        assert_eq!(alloc.free_frames(), 0);
        alloc
            .mark_free(PhysicalAddress::new(0x1000), 0x3000)
            .unwrap();
        assert_eq!(alloc.free_frames(), 3);
        let first = alloc.allocate(PageSize::Size4KiB).unwrap();
        assert_eq!(first.as_u64(), 0x1000);
        let second = alloc.allocate(PageSize::Size4KiB).unwrap();
        assert_eq!(second.as_u64(), 0x2000);
        alloc.free(first, PageSize::Size4KiB).unwrap();
        assert_eq!(
            alloc.free(first, PageSize::Size4KiB),
            Err(AllocError::DoubleFree)
        );
        assert_eq!(alloc.free_frames(), 2);
    }

    #[test]
    fn huge_page_is_aligned_and_exclusive() {
        let mut alloc = allocator(4096);
        alloc
            .mark_free(PhysicalAddress::new(0), 4096 * PAGE_SIZE)
            .unwrap();
        let huge = alloc.allocate(PageSize::Size2MiB).unwrap();
        assert!(huge.is_aligned(HUGE_PAGE_SIZE));
        assert_eq!(alloc.free_frames(), 4096 - 512);
        assert!(alloc.is_allocated(huge));
        assert!(alloc.is_allocated(PhysicalAddress::new(huge.as_u64() + PAGE_SIZE * 511)));
        let small = alloc.allocate(PageSize::Size4KiB).unwrap();
        assert!(small.as_u64() >= HUGE_PAGE_SIZE || small.as_u64() + PAGE_SIZE <= huge.as_u64());
        alloc.free(huge, PageSize::Size2MiB).unwrap();
        assert!(!alloc.is_allocated(huge));
    }

    #[test]
    fn exhaustion() {
        let mut alloc = allocator(128);
        alloc
            .mark_free(PhysicalAddress::new(0), 2 * PAGE_SIZE)
            .unwrap();
        assert!(alloc.allocate(PageSize::Size4KiB).is_ok());
        assert!(alloc.allocate(PageSize::Size4KiB).is_ok());
        assert_eq!(
            alloc.allocate(PageSize::Size4KiB),
            Err(AllocError::NoMemory)
        );
        assert_eq!(
            alloc.allocate(PageSize::Size2MiB),
            Err(AllocError::NoMemory)
        );
    }

    #[test]
    fn mark_used_is_idempotent() {
        let mut alloc = allocator(256);
        alloc
            .mark_free(PhysicalAddress::new(0), 16 * PAGE_SIZE)
            .unwrap();
        alloc
            .mark_used(PhysicalAddress::new(0x1000), 0x2000)
            .unwrap();
        alloc
            .mark_used(PhysicalAddress::new(0x1000), 0x2000)
            .unwrap();
        assert_eq!(alloc.free_frames(), 16 - 2);
        assert!(alloc.is_allocated(PhysicalAddress::new(0x1000)));
        assert!(!alloc.is_allocated(PhysicalAddress::new(0)));
    }

    #[test]
    fn short_bitmap_is_rejected() {
        let mut storage = [0u64; 1];
        let err = BitmapFrameAllocator::new(PhysicalAddress::new(0), 128, &mut storage);
        assert_eq!(err.err(), Some(AllocError::BitmapTooSmall));
    }
}
