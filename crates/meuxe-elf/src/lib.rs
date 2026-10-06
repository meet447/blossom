//! ELF64, little-endian, x86_64, `ET_EXEC`. No interpreter and no relocations.
//! The kernel maps each `PT_LOAD` into a fresh address space.

#![no_std]

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    Truncated,
    BadMagic,
    BadClass,
    BadEndian,
    BadType,
    BadMachine,
    BadPhdr,
    BadSegment,
    Empty,
}

impl ElfError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Truncated => "elf image is truncated",
            Self::BadMagic => "elf magic is wrong",
            Self::BadClass => "elf is not 64-bit",
            Self::BadEndian => "elf is not little-endian",
            Self::BadType => "elf is not an executable",
            Self::BadMachine => "elf is not x86_64",
            Self::BadPhdr => "elf program header is invalid",
            Self::BadSegment => "elf segment is invalid",
            Self::Empty => "elf has no loadable segment",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub virt: u64,
    pub offset: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub writable: bool,
    pub executable: bool,
}

pub struct ElfImage<'a> {
    bytes: &'a [u8],
    entry: u64,
    phoff: u64,
    phentsize: u16,
    phnum: u16,
}

impl<'a> ElfImage<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ElfError> {
        if bytes.len() < 64 {
            return Err(ElfError::Truncated);
        }
        if bytes[0..4] != ELF_MAGIC {
            return Err(ElfError::BadMagic);
        }
        if bytes[4] != ELFCLASS64 {
            return Err(ElfError::BadClass);
        }
        if bytes[5] != ELFDATA2LSB {
            return Err(ElfError::BadEndian);
        }
        let kind = read_u16(bytes, 16)?;
        if kind != ET_EXEC {
            return Err(ElfError::BadType);
        }
        if read_u16(bytes, 18)? != EM_X86_64 {
            return Err(ElfError::BadMachine);
        }
        let entry = read_u64(bytes, 24)?;
        let phoff = read_u64(bytes, 32)?;
        let ehsize = read_u16(bytes, 52)?;
        let phentsize = read_u16(bytes, 54)?;
        let phnum = read_u16(bytes, 56)?;
        if ehsize < 64 || phentsize != 56 || phnum == 0 || phnum > 32 {
            return Err(ElfError::BadPhdr);
        }
        let end = phoff
            .checked_add(phnum as u64 * phentsize as u64)
            .ok_or(ElfError::BadPhdr)?;
        if end > bytes.len() as u64 {
            return Err(ElfError::Truncated);
        }
        let image = Self {
            bytes,
            entry,
            phoff,
            phentsize,
            phnum,
        };
        if image.segments().next().is_none() {
            return Err(ElfError::Empty);
        }
        Ok(image)
    }

    pub fn entry(&self) -> u64 {
        self.entry
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn segments(&self) -> SegmentIter<'a> {
        SegmentIter {
            bytes: self.bytes,
            next: 0,
            phoff: self.phoff,
            phentsize: self.phentsize,
            phnum: self.phnum,
        }
    }
}

pub struct SegmentIter<'a> {
    bytes: &'a [u8],
    next: u16,
    phoff: u64,
    phentsize: u16,
    phnum: u16,
}

impl<'a> Iterator for SegmentIter<'a> {
    type Item = Result<Segment, ElfError>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.next < self.phnum {
            let index = self.next;
            self.next += 1;
            let off = self.phoff as usize + index as usize * self.phentsize as usize;
            let kind = match read_u32(self.bytes, off) {
                Ok(kind) => kind,
                Err(error) => return Some(Err(error)),
            };
            if kind != PT_LOAD {
                continue;
            }
            return Some(read_segment(self.bytes, off));
        }
        None
    }
}

fn read_segment(bytes: &[u8], off: usize) -> Result<Segment, ElfError> {
    let flags = read_u32(bytes, off + 4)?;
    let offset = read_u64(bytes, off + 8)?;
    let virt = read_u64(bytes, off + 16)?;
    let filesz = read_u64(bytes, off + 32)?;
    let memsz = read_u64(bytes, off + 40)?;
    if memsz == 0 || filesz > memsz {
        return Err(ElfError::BadSegment);
    }
    let file_end = offset.checked_add(filesz).ok_or(ElfError::BadSegment)?;
    if file_end > bytes.len() as u64 {
        return Err(ElfError::Truncated);
    }
    if virt.checked_add(memsz).is_none() {
        return Err(ElfError::BadSegment);
    }
    Ok(Segment {
        virt,
        offset,
        filesz,
        memsz,
        writable: flags & PF_W != 0,
        executable: flags & PF_X != 0,
    })
}

fn read_u16(bytes: &[u8], off: usize) -> Result<u16, ElfError> {
    let slot = bytes.get(off..off + 2).ok_or(ElfError::Truncated)?;
    Ok(u16::from_le_bytes([slot[0], slot[1]]))
}

fn read_u32(bytes: &[u8], off: usize) -> Result<u32, ElfError> {
    let slot = bytes.get(off..off + 4).ok_or(ElfError::Truncated)?;
    Ok(u32::from_le_bytes([slot[0], slot[1], slot[2], slot[3]]))
}

fn read_u64(bytes: &[u8], off: usize) -> Result<u64, ElfError> {
    let slot = bytes.get(off..off + 8).ok_or(ElfError::Truncated)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(slot);
    Ok(u64::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_exec() -> [u8; 128] {
        let mut bytes = [0u8; 128];
        bytes[0..4].copy_from_slice(&ELF_MAGIC);
        bytes[4] = ELFCLASS64;
        bytes[5] = ELFDATA2LSB;
        bytes[6] = 1;
        bytes[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
        bytes[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x400000u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        let ph = 64;
        bytes[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        bytes[ph + 4..ph + 8].copy_from_slice(&(PF_X | 4).to_le_bytes());
        bytes[ph + 8..ph + 16].copy_from_slice(&120u64.to_le_bytes());
        bytes[ph + 16..ph + 24].copy_from_slice(&0x400000u64.to_le_bytes());
        bytes[ph + 32..ph + 40].copy_from_slice(&4u64.to_le_bytes());
        bytes[ph + 40..ph + 48].copy_from_slice(&4u64.to_le_bytes());
        bytes[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes());
        bytes[120..124].copy_from_slice(&[0x90, 0x90, 0x90, 0xC3]);
        bytes
    }

    #[test]
    fn parses_one_executable_segment() {
        let bytes = minimal_exec();
        let image = ElfImage::parse(&bytes).unwrap();
        assert_eq!(image.entry(), 0x400000);
        let segment = image.segments().next().unwrap().unwrap();
        assert_eq!(segment.virt, 0x400000);
        assert_eq!(segment.filesz, 4);
        assert!(segment.executable);
        assert!(!segment.writable);
        assert_eq!(&image.bytes()[120..124], &[0x90, 0x90, 0x90, 0xC3]);
    }

    #[test]
    fn rejects_a_bad_magic() {
        let mut bytes = minimal_exec();
        bytes[0] = 0;
        assert!(matches!(ElfImage::parse(&bytes), Err(ElfError::BadMagic)));
    }
}
