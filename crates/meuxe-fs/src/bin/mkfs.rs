//! Build a 64 MiB MXDF image for the alpha disk layout.

use meuxe_fs::mxdf::{BlockDev, CREATE, Volume, BLOCK, MxError};
use std::env;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::process::ExitCode;

const ALPHA_BLOCKS: u32 = 16384;
const IMAGE_BYTES: usize = ALPHA_BLOCKS as usize * BLOCK;

struct FileDisk {
    file: File,
}

impl FileDisk {
    fn create(path: &str) -> std::io::Result<Self> {
        let file = File::create(path)?;
        file.set_len(IMAGE_BYTES as u64)?;
        Ok(Self { file })
    }
}

impl BlockDev for FileDisk {
    fn read_block(&mut self, block: u32, buf: &mut [u8; BLOCK]) -> Result<(), MxError> {
        let off = block as u64 * BLOCK as u64;
        self.file
            .seek(std::io::SeekFrom::Start(off))
            .map_err(|_| MxError::Io)?;
        self.file.read_exact(buf).map_err(|_| MxError::Io)?;
        Ok(())
    }

    fn write_block(&mut self, block: u32, buf: &mut [u8; BLOCK]) -> Result<(), MxError> {
        let off = block as u64 * BLOCK as u64;
        self.file
            .seek(std::io::SeekFrom::Start(off))
            .map_err(|_| MxError::Io)?;
        self.file.write_all(buf).map_err(|_| MxError::Io)?;
        Ok(())
    }
}

fn usage() -> ! {
    eprintln!("usage: mkfs OUT [hello=PATH] [fault=PATH]");
    std::process::exit(2);
}

fn parse_extra(arg: &str) -> Option<(&str, &str)> {
    let (key, path) = arg.split_once('=')?;
    Some((key, path))
}

fn write_file(vol: &mut Volume<FileDisk>, path: &[u8], data: &[u8]) -> Result<(), MxError> {
    vol.write_at(path, 0, data, CREATE)?;
    Ok(())
}

fn write_host_file(vol: &mut Volume<FileDisk>, dest: &[u8], host_path: &str) -> Result<(), MxError> {
    let data = std::fs::read(host_path).map_err(|_| MxError::Io)?;
    write_file(vol, dest, &data)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
    }
    let out = &args[1];
    let mut hello_src: Option<&str> = None;
    let mut fault_src: Option<&str> = None;
    for arg in args.iter().skip(2) {
        match parse_extra(arg) {
            Some(("hello", path)) => hello_src = Some(path),
            Some(("fault", path)) => fault_src = Some(path),
            _ => usage(),
        }
    }

    let disk = match FileDisk::create(out) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("mkfs: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut vol = match Volume::format(disk, ALPHA_BLOCKS, b"meuxe") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mkfs: {}", e.as_str());
            return ExitCode::FAILURE;
        }
    };

    let run = |res: Result<(), MxError>| -> ExitCode {
        match res {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mkfs: {}", e.as_str());
                ExitCode::FAILURE
            }
        }
    };

    if let Err(e) = vol.mkdir(b"/bin") {
        return run(Err(e));
    }
    if let Err(e) = vol.mkdir(b"/etc") {
        return run(Err(e));
    }
    if let Err(e) = vol.mkdir(b"/home") {
        return run(Err(e));
    }
    if let Err(e) = vol.mkdir(b"/tmp") {
        return run(Err(e));
    }
    if let Err(e) = write_file(&mut vol, b"/home/note", b"meuxe-phase3") {
        return run(Err(e));
    }
    if let Err(e) = write_file(&mut vol, b"/home/hello", b"hello") {
        return run(Err(e));
    }
    if let Some(path) = hello_src {
        if let Err(e) = write_host_file(&mut vol, b"/bin/hello", path) {
            return run(Err(e));
        }
    }
    if let Some(path) = fault_src {
        if let Err(e) = write_host_file(&mut vol, b"/bin/fault", path) {
            return run(Err(e));
        }
    }

    ExitCode::SUCCESS
}
