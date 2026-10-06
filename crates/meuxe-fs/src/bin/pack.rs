//! Pack named files into a Meuxe initramfs. Arguments are `out name=path ...`.

use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(out) = args.next() else {
        eprintln!("usage: pack OUT name=path ...");
        return ExitCode::from(1);
    };
    let mut names = Vec::new();
    let mut bodies = Vec::new();
    for spec in args {
        let Some((name, path)) = spec.split_once('=') else {
            eprintln!("expected name=path, got {spec}");
            return ExitCode::from(1);
        };
        let body = match fs::read(path) {
            Ok(body) => body,
            Err(error) => {
                eprintln!("reading {path}: {error}");
                return ExitCode::from(1);
            }
        };
        names.push(name.to_string());
        bodies.push(body);
    }
    let files: Vec<(&[u8], &[u8])> = names
        .iter()
        .zip(bodies.iter())
        .map(|(name, body)| (name.as_bytes(), body.as_slice()))
        .collect();
    let mut buf = vec![0u8; 8 + files.iter().map(|(n, d)| 8 + n.len() + d.len() + 4).sum::<usize>()];
    let len = match meuxe_fs::encode_archive(&mut buf, &files) {
        Ok(len) => len,
        Err(error) => {
            eprintln!("{}", error.as_str());
            return ExitCode::from(1);
        }
    };
    if let Err(error) = fs::write(&out, &buf[..len]) {
        eprintln!("writing {out}: {error}");
        return ExitCode::from(1);
    }
    ExitCode::from(0)
}
