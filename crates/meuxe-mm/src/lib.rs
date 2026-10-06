//! Memory primitives shared by the Meuxe kernel and host tests.
//!
//! `VirtualAddress<PagingLevel4>` is the x86_64 4-level canonical address.
//! The frame allocator is a two-level bitmap (4 KiB bits, 2 MiB groups)
//! because a Limine memory map is a sparse list of regions, not one
//! power-of-two arena a buddy allocator wants.

#![cfg_attr(not(test), no_std)]

mod addr;
mod frame;
mod heap;

pub use addr::{AddressError, PagingLevel4, PhysicalAddress, VirtualAddress};
pub use frame::{AllocError, BitmapFrameAllocator, PageSize, PAGE_SIZE, HUGE_PAGE_SIZE};
pub use heap::LinkedHeap;
