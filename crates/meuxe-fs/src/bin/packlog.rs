//! Write a two-sector append-only log into a raw disk image.

use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(out) = args.next() else {
        eprintln!("usage: packlog OUT");
        return ExitCode::from(1);
    };
    let mut disk = vec![0u8; 64 * 1024];
    let pad = vec![b'.'; 500];
    let records: [(&[u8], &[u8]); 3] = [
        (b"note", b"meuxe-phase3"),
        (b"hello", b"hello"),
        (b"disk", pad.as_slice()),
    ];
    if let Err(error) = meuxe_fs::encode_log(&mut disk[..1024], &records) {
        eprintln!("{}", error.as_str());
        return ExitCode::from(1);
    }
    if let Err(error) = fs::write(&out, &disk) {
        eprintln!("writing {out}: {error}");
        return ExitCode::from(1);
    }
    ExitCode::from(0)
}
