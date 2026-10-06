//! First-fit heap with address-ordered coalescing.
//!
//! Every live allocation is preceded by a 32-byte header. Payloads are at
//! least 16-byte aligned. The kernel wraps this in a lock and installs it
//! as the global allocator.

use core::alloc::Layout;
use core::ptr;

const HEADER_SIZE: usize = 32;

#[repr(C, align(16))]
struct Header {
    size: usize,
    free: usize,
    next: *mut Header,
    prev: *mut Header,
}

const _: () = assert!(core::mem::size_of::<Header>() == HEADER_SIZE);

pub struct LinkedHeap {
    free_head: *mut Header,
    start: usize,
    end: usize,
}

unsafe impl Send for LinkedHeap {}

impl LinkedHeap {
    /// `mem` must be 16-byte aligned and exclusively owned for the heap's life.
    ///
    /// # Safety
    /// `mem` .. `mem + len` must be writable and not aliased.
    pub unsafe fn init(mem: *mut u8, len: usize) -> Result<Self, ()> {
        let start = mem as usize;
        if start % 16 != 0 || len < HEADER_SIZE + 16 {
            return Err(());
        }
        let len = len & !15;
        let hdr = mem as *mut Header;
        ptr::write(
            hdr,
            Header {
                size: len - HEADER_SIZE,
                free: 1,
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
            },
        );
        Ok(Self {
            free_head: hdr,
            start,
            end: start + len,
        })
    }

    pub fn free_payload(&self) -> usize {
        let mut total = 0;
        let mut cursor = self.free_head;
        while !cursor.is_null() {
            total += unsafe { (*cursor).size };
            cursor = unsafe { (*cursor).next };
        }
        total
    }

    /// # Safety
    /// The caller has exclusive access to this heap.
    pub unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(16);
        let need = align_up(layout.size().max(1), 16);
        let mut cursor = self.free_head;
        while !cursor.is_null() {
            if let Some(ptr) = self.try_block(cursor, need, align) {
                return ptr;
            }
            cursor = unsafe { (*cursor).next };
        }
        ptr::null_mut()
    }

    /// # Safety
    /// `ptr` came from [`Self::alloc`] on this heap and has not been freed.
    pub unsafe fn dealloc(&mut self, ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }
        let hdr = (ptr as usize - HEADER_SIZE) as *mut Header;
        let addr = hdr as usize;
        if addr < self.start || addr >= self.end {
            panic!("meuxe heap: pointer outside heap");
        }
        if unsafe { (*hdr).free } == 1 {
            panic!("meuxe heap: double free");
        }
        self.insert_free(hdr);
        self.coalesce(hdr);
    }

    unsafe fn try_block(&mut self, hdr: *mut Header, need: usize, align: usize) -> Option<*mut u8> {
        let block = hdr as usize;
        let old_size = unsafe { (*hdr).size };
        let end = block + HEADER_SIZE + old_size;
        let payload = align_up(block + HEADER_SIZE, align);
        if payload < block + HEADER_SIZE {
            return None;
        }
        let header_at = payload - HEADER_SIZE;
        if header_at < block {
            return None;
        }
        let gap = header_at - block;
        if gap != 0 && gap < HEADER_SIZE {
            return None;
        }
        if payload.checked_add(need)? > end {
            return None;
        }

        if gap == 0 {
            self.unlink(hdr);
            unsafe {
                (*hdr).free = 0;
                (*hdr).next = ptr::null_mut();
                (*hdr).prev = ptr::null_mut();
            }
            self.split_tail(hdr, need);
            Some(payload as *mut u8)
        } else {
            unsafe {
                (*hdr).size = gap - HEADER_SIZE;
                let new = header_at as *mut Header;
                ptr::write(
                    new,
                    Header {
                        size: end - payload,
                        free: 0,
                        next: ptr::null_mut(),
                        prev: ptr::null_mut(),
                    },
                );
                self.split_tail(new, need);
                Some(payload as *mut u8)
            }
        }
    }

    unsafe fn split_tail(&mut self, hdr: *mut Header, need: usize) {
        let payload = hdr as usize + HEADER_SIZE;
        let end = payload + unsafe { (*hdr).size };
        let tail = payload + need;
        if end - tail >= HEADER_SIZE {
            let tail_hdr = tail as *mut Header;
            unsafe {
                ptr::write(
                    tail_hdr,
                    Header {
                        size: end - tail - HEADER_SIZE,
                        free: 1,
                        next: ptr::null_mut(),
                        prev: ptr::null_mut(),
                    },
                );
                (*hdr).size = need;
            }
            self.insert_free(tail_hdr);
        }
    }

    fn insert_free(&mut self, hdr: *mut Header) {
        unsafe {
            (*hdr).free = 1;
        }
        let addr = hdr as usize;
        let mut prev = ptr::null_mut();
        let mut cursor = self.free_head;
        while !cursor.is_null() && (cursor as usize) < addr {
            prev = cursor;
            cursor = unsafe { (*cursor).next };
        }
        unsafe {
            (*hdr).next = cursor;
            (*hdr).prev = prev;
            if prev.is_null() {
                self.free_head = hdr;
            } else {
                (*prev).next = hdr;
            }
            if !cursor.is_null() {
                (*cursor).prev = hdr;
            }
        }
    }

    fn coalesce(&mut self, hdr: *mut Header) {
        unsafe {
            let next = (*hdr).next;
            if !next.is_null() {
                let hdr_end = hdr as usize + HEADER_SIZE + (*hdr).size;
                if hdr_end == next as usize {
                    (*hdr).size += HEADER_SIZE + (*next).size;
                    self.unlink(next);
                }
            }
            let prev = (*hdr).prev;
            if !prev.is_null() {
                let prev_end = prev as usize + HEADER_SIZE + (*prev).size;
                if prev_end == hdr as usize {
                    (*prev).size += HEADER_SIZE + (*hdr).size;
                    self.unlink(hdr);
                }
            }
        }
    }

    fn unlink(&mut self, hdr: *mut Header) {
        unsafe {
            let prev = (*hdr).prev;
            let next = (*hdr).next;
            if prev.is_null() {
                self.free_head = next;
            } else {
                (*prev).next = next;
            }
            if !next.is_null() {
                (*next).prev = prev;
            }
            (*hdr).next = ptr::null_mut();
            (*hdr).prev = ptr::null_mut();
        }
    }
}

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C, align(4096))]
    struct Region(pub [u8; 64 * 1024]);

    #[test]
    fn allocates_distinct_blocks_and_coalesces() {
        let mut region = Region([0; 64 * 1024]);
        let mut heap = unsafe { LinkedHeap::init(region.0.as_mut_ptr(), region.0.len()).unwrap() };
        let initial = heap.free_payload();
        let layout = Layout::from_size_align(200, 8).unwrap();
        let mut ptrs = [ptr::null_mut(); 32];
        for slot in &mut ptrs {
            *slot = unsafe { heap.alloc(layout) };
            assert!(!slot.is_null());
            unsafe { ptr::write_bytes(*slot, 0xAB, 200) };
        }
        let big_layout = Layout::from_size_align(4096, 64).unwrap();
        let aligned = unsafe { heap.alloc(big_layout) };
        assert!(!aligned.is_null());
        assert_eq!(aligned as usize & 63, 0);
        unsafe { heap.dealloc(aligned) };
        for slot in ptrs {
            unsafe { heap.dealloc(slot) };
        }
        assert_eq!(heap.free_payload(), initial);
        let huge = unsafe { heap.alloc(Layout::from_size_align(32 * 1024, 16).unwrap()) };
        assert!(!huge.is_null());
    }

    #[test]
    fn out_of_memory_returns_null() {
        let mut region = Region([0; 64 * 1024]);
        let mut heap = unsafe { LinkedHeap::init(region.0.as_mut_ptr(), 4096).unwrap() };
        let ptr = unsafe { heap.alloc(Layout::from_size_align(1024 * 1024, 16).unwrap()) };
        assert!(ptr.is_null());
    }

    #[test]
    #[should_panic]
    fn double_free_panics() {
        let mut region = Region([0; 64 * 1024]);
        let mut heap = unsafe { LinkedHeap::init(region.0.as_mut_ptr(), region.0.len()).unwrap() };
        let ptr = unsafe { heap.alloc(Layout::from_size_align(32, 8).unwrap()) };
        unsafe {
            heap.dealloc(ptr);
            heap.dealloc(ptr);
        }
    }
}
