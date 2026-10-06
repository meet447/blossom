//! Two packed formats.
//!
//! The initramfs is how the kernel receives ELF images. The append-only log
//! is a flat directory of named records: a magic, a version, then name/data
//! pairs. `write_names` is the listing a shell prints for `ls`.

#![no_std]

pub mod mxdf;

pub const ARCHIVE_MAGIC: u32 = 0x5346_584D;
pub const LOG_MAGIC: u32 = 0x474C_584D;
pub const LOG_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    Truncated,
    BadMagic,
    BadVersion,
    BadName,
}

impl FsError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Truncated => "filesystem image is truncated",
            Self::BadMagic => "filesystem magic is wrong",
            Self::BadVersion => "filesystem version is wrong",
            Self::BadName => "filesystem record name is empty",
        }
    }
}

pub struct Archive<'a> {
    bytes: &'a [u8],
}

impl<'a> Archive<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FsError> {
        if bytes.len() < 8 {
            return Err(FsError::Truncated);
        }
        if read_u32(bytes, 0)? != ARCHIVE_MAGIC {
            return Err(FsError::BadMagic);
        }
        let count = read_u32(bytes, 4)? as usize;
        let mut cursor = 8usize;
        for _ in 0..count {
            if cursor + 8 > bytes.len() {
                return Err(FsError::Truncated);
            }
            let name_len = read_u32(bytes, cursor)? as usize;
            let data_len = read_u32(bytes, cursor + 4)? as usize;
            if name_len == 0 || name_len > 64 {
                return Err(FsError::BadName);
            }
            let body = cursor + 8 + name_len + data_len;
            let next = align4(body);
            if next > bytes.len() {
                return Err(FsError::Truncated);
            }
            cursor = next;
        }
        Ok(Self { bytes })
    }

    pub fn lookup(&self, name: &[u8]) -> Option<&'a [u8]> {
        let count = read_u32(self.bytes, 4).ok()? as usize;
        let mut cursor = 8usize;
        for _ in 0..count {
            let name_len = read_u32(self.bytes, cursor).ok()? as usize;
            let data_len = read_u32(self.bytes, cursor + 4).ok()? as usize;
            let name_bytes = &self.bytes[cursor + 8..cursor + 8 + name_len];
            let data = &self.bytes[cursor + 8 + name_len..cursor + 8 + name_len + data_len];
            if name_bytes == name {
                return Some(data);
            }
            cursor = align4(cursor + 8 + name_len + data_len);
        }
        None
    }
}

pub struct Log<'a> {
    bytes: &'a [u8],
}

impl<'a> Log<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FsError> {
        if bytes.len() < 8 {
            return Err(FsError::Truncated);
        }
        if read_u32(bytes, 0)? != LOG_MAGIC {
            return Err(FsError::BadMagic);
        }
        if read_u32(bytes, 4)? != LOG_VERSION {
            return Err(FsError::BadVersion);
        }
        let mut cursor = 8usize;
        while cursor + 4 <= bytes.len() {
            let name_len = read_u16(bytes, cursor)? as usize;
            let data_len = read_u16(bytes, cursor + 2)? as usize;
            if name_len == 0 {
                break;
            }
            if name_len > 64 {
                return Err(FsError::BadName);
            }
            let next = cursor + 4 + name_len + data_len;
            if next > bytes.len() {
                return Err(FsError::Truncated);
            }
            cursor = next;
        }
        Ok(Self { bytes })
    }

    pub fn lookup(&self, name: &[u8]) -> Option<&'a [u8]> {
        let mut cursor = 8usize;
        while cursor + 4 <= self.bytes.len() {
            let name_len = read_u16(self.bytes, cursor).ok()? as usize;
            let data_len = read_u16(self.bytes, cursor + 2).ok()? as usize;
            if name_len == 0 {
                break;
            }
            let name_bytes = &self.bytes[cursor + 4..cursor + 4 + name_len];
            let data = &self.bytes[cursor + 4 + name_len..cursor + 4 + name_len + data_len];
            if name_bytes == name {
                return Some(data);
            }
            cursor += 4 + name_len + data_len;
        }
        None
    }

    /// Space-separated record names, in log order. Names stop at `dst`.
    pub fn write_names(&self, dst: &mut [u8]) -> usize {
        let mut cursor = 8usize;
        let mut written = 0usize;
        let mut first = true;
        while cursor + 4 <= self.bytes.len() && written < dst.len() {
            let Ok(name_len) = read_u16(self.bytes, cursor) else {
                break;
            };
            let Ok(data_len) = read_u16(self.bytes, cursor + 2) else {
                break;
            };
            let name_len = name_len as usize;
            let data_len = data_len as usize;
            if name_len == 0 {
                break;
            }
            let next = cursor + 4 + name_len + data_len;
            if next > self.bytes.len() {
                break;
            }
            if !first {
                dst[written] = b' ';
                written += 1;
                if written == dst.len() {
                    break;
                }
            }
            first = false;
            let name = &self.bytes[cursor + 4..cursor + 4 + name_len];
            let copy = name_len.min(dst.len() - written);
            dst[written..written + copy].copy_from_slice(&name[..copy]);
            written += copy;
            cursor = next;
        }
        written
    }
}

/// Append one record in front of the terminator. Earlier bytes stay put,
/// including whatever record is at the start of the log.
pub fn append_record(bytes: &mut [u8], name: &[u8], data: &[u8]) -> Result<(), FsError> {
    if bytes.len() < 8 || read_u32(bytes, 0)? != LOG_MAGIC {
        return Err(FsError::BadMagic);
    }
    if read_u32(bytes, 4)? != LOG_VERSION {
        return Err(FsError::BadVersion);
    }
    if name.is_empty() || name.len() > 64 {
        return Err(FsError::BadName);
    }
    if data.len() > u16::MAX as usize {
        return Err(FsError::Truncated);
    }
    let mut cursor = 8usize;
    while cursor + 4 <= bytes.len() {
        let name_len = read_u16(bytes, cursor)? as usize;
        let data_len = read_u16(bytes, cursor + 2)? as usize;
        if name_len == 0 {
            let next = cursor + 4 + name.len() + data.len();
            if next > bytes.len() {
                return Err(FsError::Truncated);
            }
            write_u16(bytes, cursor, name.len() as u16);
            write_u16(bytes, cursor + 2, data.len() as u16);
            bytes[cursor + 4..cursor + 4 + name.len()].copy_from_slice(name);
            bytes[cursor + 4 + name.len()..next].copy_from_slice(data);
            if next + 4 <= bytes.len() {
                write_u16(bytes, next, 0);
                write_u16(bytes, next + 2, 0);
            }
            return Ok(());
        }
        if name_len > 64 {
            return Err(FsError::BadName);
        }
        let next = cursor + 4 + name_len + data_len;
        if next > bytes.len() {
            return Err(FsError::Truncated);
        }
        cursor = next;
    }
    Err(FsError::Truncated)
}

pub fn encode_archive(dst: &mut [u8], files: &[(&[u8], &[u8])]) -> Result<usize, FsError> {
    if dst.len() < 8 {
        return Err(FsError::Truncated);
    }
    write_u32(dst, 0, ARCHIVE_MAGIC);
    write_u32(dst, 4, files.len() as u32);
    let mut cursor = 8usize;
    for (name, data) in files {
        if name.is_empty() || name.len() > 64 {
            return Err(FsError::BadName);
        }
        let next = align4(cursor + 8 + name.len() + data.len());
        if next > dst.len() {
            return Err(FsError::Truncated);
        }
        write_u32(dst, cursor, name.len() as u32);
        write_u32(dst, cursor + 4, data.len() as u32);
        dst[cursor + 8..cursor + 8 + name.len()].copy_from_slice(name);
        dst[cursor + 8 + name.len()..cursor + 8 + name.len() + data.len()].copy_from_slice(data);
        for byte in &mut dst[cursor + 8 + name.len() + data.len()..next] {
            *byte = 0;
        }
        cursor = next;
    }
    Ok(cursor)
}

pub fn encode_log(dst: &mut [u8], records: &[(&[u8], &[u8])]) -> Result<usize, FsError> {
    if dst.len() < 8 {
        return Err(FsError::Truncated);
    }
    write_u32(dst, 0, LOG_MAGIC);
    write_u32(dst, 4, LOG_VERSION);
    let mut cursor = 8usize;
    for (name, data) in records {
        if name.is_empty() || name.len() > 64 || name.len() > u16::MAX as usize {
            return Err(FsError::BadName);
        }
        if data.len() > u16::MAX as usize {
            return Err(FsError::Truncated);
        }
        let next = cursor + 4 + name.len() + data.len();
        if next > dst.len() {
            return Err(FsError::Truncated);
        }
        write_u16(dst, cursor, name.len() as u16);
        write_u16(dst, cursor + 2, data.len() as u16);
        dst[cursor + 4..cursor + 4 + name.len()].copy_from_slice(name);
        dst[cursor + 4 + name.len()..next].copy_from_slice(data);
        cursor = next;
    }
    if cursor + 4 <= dst.len() {
        write_u16(dst, cursor, 0);
        write_u16(dst, cursor + 2, 0);
    }
    Ok(cursor)
}

fn align4(value: usize) -> usize {
    value.wrapping_add(3) & !3
}

fn read_u16(bytes: &[u8], off: usize) -> Result<u16, FsError> {
    let slot = bytes.get(off..off + 2).ok_or(FsError::Truncated)?;
    Ok(u16::from_le_bytes([slot[0], slot[1]]))
}

fn read_u32(bytes: &[u8], off: usize) -> Result<u32, FsError> {
    let slot = bytes.get(off..off + 4).ok_or(FsError::Truncated)?;
    Ok(u32::from_le_bytes([slot[0], slot[1], slot[2], slot[3]]))
}

fn write_u16(bytes: &mut [u8], off: usize, value: u16) {
    bytes[off..off + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], off: usize, value: u32) {
    bytes[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_round_trip() {
        let mut buf = [0u8; 128];
        let len = encode_archive(&mut buf, &[(b"vfs", b"elf-a"), (b"blk", b"elf-b")]).unwrap();
        let archive = Archive::parse(&buf[..len]).unwrap();
        assert_eq!(archive.lookup(b"vfs"), Some(&b"elf-a"[..]));
        assert_eq!(archive.lookup(b"blk"), Some(&b"elf-b"[..]));
        assert_eq!(archive.lookup(b"missing"), None);
    }

    #[test]
    fn log_finds_a_named_record() {
        let mut buf = [0u8; 64];
        encode_log(&mut buf, &[(b"note", b"meuxe-phase3")]).unwrap();
        let log = Log::parse(&buf).unwrap();
        assert_eq!(log.lookup(b"note"), Some(&b"meuxe-phase3"[..]));
        assert_eq!(log.lookup(b"other"), None);
    }

    #[test]
    fn log_lists_records_that_cross_a_sector() {
        let mut buf = [0u8; 1024];
        let pad = [b'.'; 500];
        let len = encode_log(
            &mut buf,
            &[
                (b"note", b"meuxe-phase3"),
                (b"hello", b"hello"),
                (b"disk", &pad),
            ],
        )
        .unwrap();
        assert!(len > 512);
        let log = Log::parse(&buf).unwrap();
        let mut names = [0u8; 32];
        let count = log.write_names(&mut names);
        assert_eq!(&names[..count], b"note hello disk");
        assert_eq!(log.lookup(b"note"), Some(&b"meuxe-phase3"[..]));
        assert_eq!(log.lookup(b"hello"), Some(&b"hello"[..]));
        assert_eq!(log.lookup(b"disk").map(|data| data.len()), Some(500));
        let mut prefix = [0u8; 28];
        prefix.copy_from_slice(&buf[..28]);
        append_record(&mut buf, b"hi", b"there").unwrap();
        assert_eq!(&buf[..28], &prefix);
        let log = Log::parse(&buf).unwrap();
        assert_eq!(log.lookup(b"note"), Some(&b"meuxe-phase3"[..]));
        assert_eq!(log.lookup(b"hi"), Some(&b"there"[..]));
        let mut names = [0u8; 32];
        let count = log.write_names(&mut names);
        assert_eq!(&names[..count], b"note hello disk hi");
    }

    #[test]
    fn log_rejects_a_bad_magic() {
        let mut buf = [0u8; 64];
        encode_log(&mut buf, &[(b"note", b"x")]).unwrap();
        buf[0] = 0;
        assert!(matches!(Log::parse(&buf), Err(FsError::BadMagic)));
    }
}
