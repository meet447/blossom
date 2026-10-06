//! Bootstrap capability space.
//!
//! Handles are slot indexes. Task 9 (the ring-3 proof) receives an endpoint
//! and the frame that backs its ring page. A second task receives a read-only
//! mint, which cannot grow its rights.

use crate::task::{MINT_TARGET, USER_STUB};
use crate::sync::Mutex;
use meuxe_cap::{CapError, CapSpace, ObjectKind, TaskId};

pub use meuxe_abi::{CapHandle, CapType, Rights};

static CAPS: Mutex<CapSpace> = Mutex::new(CapSpace::new());

const _: () = assert!(Rights::READ.bits() == 1);
const _: () = assert!(Rights::GRANT.bits() == 1 << 3);
const _: () = assert!(CapHandle::NULL.raw() == 0);
const _: () = assert!(CapType::Mmio as u8 == 6);

pub fn init(ring_phys: u64) -> Result<(), &'static str> {
    let user = TaskId::new(USER_STUB).ok_or("user task id is out of range")?;
    let other = TaskId::new(MINT_TARGET).ok_or("mint target is out of range")?;
    let mut caps = CAPS.lock();
    let endpoint = caps
        .create(ObjectKind::Endpoint { parked: None })
        .map_err(|_| "endpoint object table is full")?;
    let frame = caps
        .create(ObjectKind::Frame { phys: ring_phys })
        .map_err(|_| "frame object table is full")?;
    let endpoint_handle = caps
        .install(
            user,
            endpoint,
            Rights::READ.union(Rights::WRITE).union(Rights::GRANT),
        )
        .map_err(|_| "installing the endpoint capability failed")?;
    let frame_handle = caps
        .install(user, frame, Rights::READ.union(Rights::WRITE))
        .map_err(|_| "installing the frame capability failed")?;
    let minted = caps
        .mint(user, endpoint_handle, other, Rights::READ)
        .map_err(|_| "minting a read capability failed")?;
    let rejected = caps.mint(
        user,
        endpoint_handle,
        other,
        Rights::READ.union(Rights::EXECUTE),
    );
    if rejected != Err(CapError::RightsExceeded) {
        return Err("mint accepted rights the source does not have");
    }
    crate::ipc::register_ring(USER_STUB, ring_phys);
    if endpoint_handle.raw() != 1 || frame_handle.raw() != 2 {
        return Err("user capability handles are not 1 and 2");
    }
    crate::kprintln!(
        "meuxe: caps endpoint={} frame={} minted={} mint_subset=ok",
        endpoint_handle.raw(),
        frame_handle.raw(),
        minted.raw()
    );
    Ok(())
}

pub fn with_mut<R>(body: impl FnOnce(&mut CapSpace) -> R) -> R {
    body(&mut CAPS.lock())
}
