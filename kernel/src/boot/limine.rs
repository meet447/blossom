//! Limine boot protocol, base revision 6.
//!
//! Requests live between the start and end markers. The bootloader writes the
//! response pointer; we only read it with a volatile load.

use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::AtomicU64;

pub const COMMON_MAGIC: [u64; 2] = [0xc7b1dd30df4c8b88, 0x0a82e883a194f07b];

pub const MEMMAP_USABLE: u64 = 0;
pub const MEMMAP_RESERVED: u64 = 1;
pub const MEMMAP_ACPI_RECLAIMABLE: u64 = 2;
pub const MEMMAP_ACPI_NVS: u64 = 3;
pub const MEMMAP_BAD_MEMORY: u64 = 4;
pub const MEMMAP_BOOTLOADER_RECLAIMABLE: u64 = 5;
pub const MEMMAP_EXECUTABLE_AND_MODULES: u64 = 6;
pub const MEMMAP_FRAMEBUFFER: u64 = 7;
pub const MEMMAP_RESERVED_MAPPED: u64 = 8;

#[repr(C, align(8))]
pub struct BaseRevision {
    magic: UnsafeCell<[u64; 3]>,
}

unsafe impl Sync for BaseRevision {}

impl BaseRevision {
    pub const fn new(revision: u64) -> Self {
        Self {
            magic: UnsafeCell::new([0xf9562b2d5c95a6c8, 0x6a7b384944536bdc, revision]),
        }
    }

    pub fn is_supported(&self) -> bool {
        unsafe { self.magic.get().cast::<u64>().add(2).read_volatile() == 0 }
    }

    pub fn loaded_revision(&self) -> Option<u64> {
        let word = unsafe { self.magic.get().cast::<u64>().add(1).read_volatile() };
        if word == 0x6a7b384944536bdc {
            None
        } else {
            Some(word)
        }
    }
}

#[repr(C, align(8))]
pub struct RequestsStartMarker([u64; 4]);

impl RequestsStartMarker {
    pub const fn new() -> Self {
        Self([
            0xf6b8f4b39de7d1ae,
            0xfab91a6940fcb9cf,
            0x785c6ed015d3e316,
            0x181e920a7852b9d9,
        ])
    }
}

#[repr(C, align(8))]
pub struct RequestsEndMarker([u64; 2]);

impl RequestsEndMarker {
    pub const fn new() -> Self {
        Self([0xadc0e0531bb10d03, 0x9572709f31764c62])
    }
}

#[repr(C)]
pub struct Response<T> {
    pub revision: u64,
    pub data: T,
}

#[repr(C, align(8))]
pub struct Request<T> {
    magic: [u64; 2],
    id: [u64; 2],
    revision: u64,
    response: UnsafeCell<*mut Response<T>>,
}

unsafe impl<T> Sync for Request<T> {}

impl<T> Request<T> {
    pub const fn new(id: [u64; 2]) -> Self {
        Self {
            magic: COMMON_MAGIC,
            id,
            revision: 0,
            response: UnsafeCell::new(ptr::null_mut()),
        }
    }

    pub fn get(&self) -> Option<&T> {
        let response = unsafe { self.response.get().read_volatile() };
        if response.is_null() {
            None
        } else {
            Some(unsafe { &(*response).data })
        }
    }
}

#[repr(C, align(8))]
pub struct RequestU64<T> {
    magic: [u64; 2],
    id: [u64; 2],
    revision: u64,
    response: UnsafeCell<*mut Response<T>>,
    value: u64,
}

unsafe impl<T> Sync for RequestU64<T> {}

impl<T> RequestU64<T> {
    pub const fn new(id: [u64; 2], value: u64) -> Self {
        Self {
            magic: COMMON_MAGIC,
            id,
            revision: 0,
            response: UnsafeCell::new(ptr::null_mut()),
            value,
        }
    }

    #[allow(dead_code)]
    pub fn get(&self) -> Option<&T> {
        let response = unsafe { self.response.get().read_volatile() };
        if response.is_null() {
            None
        } else {
            Some(unsafe { &(*response).data })
        }
    }
}

#[repr(C, align(8))]
pub struct PagingModeRequest {
    magic: [u64; 2],
    id: [u64; 2],
    revision: u64,
    response: UnsafeCell<*mut Response<PagingMode>>,
    mode: u64,
    max_mode: u64,
    min_mode: u64,
}

unsafe impl Sync for PagingModeRequest {}

#[repr(C)]
pub struct PagingMode {
    pub mode: u64,
}

impl PagingModeRequest {
    pub const fn exact_4level() -> Self {
        Self {
            magic: COMMON_MAGIC,
            id: [0x95c1a0edab0944cb, 0xa4e5cb3842f7488a],
            revision: 0,
            response: UnsafeCell::new(ptr::null_mut()),
            mode: 0,
            max_mode: 0,
            min_mode: 0,
        }
    }

    pub fn mode(&self) -> Option<u64> {
        let response = unsafe { self.response.get().read_volatile() };
        if response.is_null() {
            None
        } else {
            Some(unsafe { (*response).data.mode })
        }
    }
}

#[repr(C)]
pub struct BootloaderInfo {
    pub name: *const u8,
    pub version: *const u8,
}

#[repr(C)]
pub struct FirmwareType {
    pub firmware_type: u64,
}

#[repr(C)]
pub struct Hhdm {
    pub offset: u64,
}

#[repr(C)]
pub struct ExecutableAddress {
    pub physical_base: u64,
    pub virtual_base: u64,
}

#[repr(C)]
pub struct MemmapEntry {
    pub base: u64,
    pub length: u64,
    pub kind: u64,
}

#[repr(C)]
pub struct Memmap {
    entry_count: u64,
    entries: *const *const MemmapEntry,
}

impl Memmap {
    pub fn entries(&self) -> &[&MemmapEntry] {
        unsafe { &*ptr::slice_from_raw_parts(self.entries as *const &MemmapEntry, self.entry_count as usize) }
    }
}

#[repr(C)]
pub struct Framebuffer {
    pub address: *mut u8,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
    pub unused: [u8; 7],
    pub edid_size: u64,
    pub edid: *const u8,
}

#[repr(C)]
pub struct FramebufferList {
    count: u64,
    framebuffers: *const *const Framebuffer,
}

impl FramebufferList {
    pub fn first(&self) -> Option<&Framebuffer> {
        if self.count == 0 || self.framebuffers.is_null() {
            None
        } else {
            Some(unsafe { &*(*self.framebuffers) })
        }
    }
}

#[repr(C)]
pub struct Rsdp {
    pub address: *mut u8,
}

#[repr(C)]
pub struct TscFrequency {
    pub frequency: u64,
}

#[repr(C)]
pub struct MpResponse {
    pub flags: u32,
    pub bsp_lapic_id: u32,
    pub cpu_count: u64,
    pub cpus: *const *const MpInfo,
}

#[repr(C)]
pub struct MpInfo {
    pub processor_id: u32,
    pub lapic_id: u32,
    pub reserved: u64,
    pub goto_address: AtomicU64,
    pub extra_argument: AtomicU64,
}

const _: () = assert!(core::mem::offset_of!(MpInfo, extra_argument) == 24);

#[used]
#[link_section = ".requests_start"]
pub static START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[link_section = ".requests"]
pub static BASE_REVISION: BaseRevision = BaseRevision::new(6);

#[used]
#[link_section = ".requests"]
pub static BOOTLOADER_INFO: Request<BootloaderInfo> =
    Request::new([0xf55038d8e2a1202f, 0x279426fcf5f59740]);

#[used]
#[link_section = ".requests"]
pub static FIRMWARE: Request<FirmwareType> = Request::new([0x8c2f75d90bef28a8, 0x7045a4688eac00c3]);

#[used]
#[link_section = ".requests"]
pub static STACK: RequestU64<()> = RequestU64::new([0x224ef0460a8e8926, 0xe1cb0fc25f46ea3d], 256 * 1024);

#[used]
#[link_section = ".requests"]
pub static HHDM: Request<Hhdm> = Request::new([0x48dcf1cb8ad2b852, 0x63984e959a98244b]);

#[used]
#[link_section = ".requests"]
pub static FRAMEBUFFER: Request<FramebufferList> =
    Request::new([0x9d5827dcd881dd75, 0xa3148604f6fab11b]);

#[used]
#[link_section = ".requests"]
pub static PAGING_MODE: PagingModeRequest = PagingModeRequest::exact_4level();

#[used]
#[link_section = ".requests"]
pub static MEMMAP: Request<Memmap> = Request::new([0x67cf3d9d378a806f, 0xe304acdfc50c3c62]);

#[used]
#[link_section = ".requests"]
pub static RSDP: Request<Rsdp> = Request::new([0xc5e77b6b397e7b43, 0x27637845accdcf3c]);

#[used]
#[link_section = ".requests"]
pub static EXECUTABLE_ADDRESS: Request<ExecutableAddress> =
    Request::new([0x71ba76863cc55f63, 0xb2644a48c516a487]);

#[used]
#[link_section = ".requests"]
pub static TSC: Request<TscFrequency> = Request::new([0x10f2ee1d87d195e4, 0xf747a2b78f6ddb31]);

#[used]
#[link_section = ".requests"]
pub static MP: RequestU64<MpResponse> =
    RequestU64::new([0x95a67b819a1b857e, 0xa0b61b723b6a73e0], 0);

#[used]
#[link_section = ".requests_end"]
pub static END_MARKER: RequestsEndMarker = RequestsEndMarker::new();
