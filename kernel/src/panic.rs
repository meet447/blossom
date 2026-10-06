//! Kernel panic. Serial only: the console lock may already be held.

use crate::arch::{debug_exit, halt_forever};
use crate::arch::x86_64::serial::Writer;
use core::fmt::Write;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let _ = write!(Writer, "\nmeuxe: panic: {info}\n");
    if cfg!(feature = "verify") {
        debug_exit(0x02);
    }
    halt_forever();
}
