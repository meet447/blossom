//! Serial log, mirrored to the framebuffer once the console exists.

use crate::arch::x86_64::serial;
use crate::console;
use crate::sync::Mutex;
use core::fmt;

static PRINT: Mutex<()> = Mutex::new(());

#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {{
        $crate::log::_print(format_args!("{}\n", format_args!($($arg)*)));
    }};
}

pub fn _print(args: fmt::Arguments) {
    let _guard = PRINT.lock();
    let _ = fmt::Write::write_fmt(&mut serial::Writer, args);
    console::write_fmt(args);
}

pub fn fault(vector: u64, error: u64, rip: u64, cr2: u64) {
    use core::fmt::Write;
    let _ = write!(
        serial::Writer,
        "\nmeuxe: exception vector={vector:#x} error={error:#x} rip={rip:#x} cr2={cr2:#x}\n"
    );
}
