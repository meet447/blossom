//! MXDF block filesystem (no_std, no alloc).

pub const BLOCK: usize = 4096;
pub const MAGIC: u32 = 0x4644_584D;
pub const VERSION: u32 = 1;

pub const INODE_SIZE: usize = 64;
pub const INODES_PER_BLOCK: usize = BLOCK / INODE_SIZE;
pub const DIR_ENTRY_SIZE: usize = 32;
pub const ENTRIES_PER_BLOCK: usize = BLOCK / DIR_ENTRY_SIZE;
pub const MAX_DIRECT: usize = 8;
pub const INDIRECT_PTRS: usize = BLOCK / 4;
pub const MAX_PATH: usize = 128;
pub const MAX_DEPTH: usize = 8;
pub const MAX_NAME: usize = 24;

pub const CREATE: u32 = 1;
pub const TRUNC: u32 = 2;
pub const APPEND: u32 = 4;

const INODE_COUNT: u32 = 256;
const INODE_TABLE_BLOCKS: u32 = 4;
const BITMAP_BLOCK: u32 = 1;
const INODE_TABLE_BLOCK: u32 = 2;
const DATA_FIRST: u32 = 6;
const ROOT_INO: u32 = 1;
const MAX_BITMAP_BLOCKS: u32 = 32768;

const INODE_KIND_FILE: u16 = 1;
const INODE_KIND_DIR: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MxError {
    BadMagic,
    BadVersion,
    NotFound,
    Exists,
    NotEmpty,
    Invalid,
    NoSpace,
    IsDir,
    NotDir,
    Io,
}

impl MxError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadMagic => "filesystem magic is wrong",
            Self::BadVersion => "filesystem version is wrong",
            Self::NotFound => "path not found",
            Self::Exists => "path already exists",
            Self::NotEmpty => "directory is not empty",
            Self::Invalid => "invalid path or argument",
            Self::NoSpace => "filesystem is full",
            Self::IsDir => "path is a directory",
            Self::NotDir => "path is not a directory",
            Self::Io => "block device I/O error",
        }
    }
}

pub trait BlockDev {
    fn read_block(&mut self, block: u32, buf: &mut [u8; BLOCK]) -> Result<(), MxError>;
    fn write_block(&mut self, block: u32, buf: &mut [u8; BLOCK]) -> Result<(), MxError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: u16,
    pub size: u32,
    pub nlink: u16,
    pub mtime: u64,
    pub ino: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsStat {
    pub total_blocks: u32,
    pub free_blocks: u32,
    pub inode_count: u32,
    pub mount_count: u32,
}

struct Superblock {
    magic: u32,
    version: u32,
    block_size: u32,
    total_blocks: u32,
    inode_count: u32,
    bitmap_block: u32,
    inode_table_block: u32,
    inode_table_blocks: u32,
    data_first: u32,
    root_ino: u32,
    mount_count: u32,
    free_blocks: u32,
    label: [u8; 16],
}

struct Inode {
    kind: u16,
    nlink: u16,
    size: u32,
    mtime: u64,
    direct: [u32; MAX_DIRECT],
    indirect: u32,
}

pub struct Volume<D: BlockDev> {
    dev: D,
    sb: Superblock,
    block_buf: [u8; BLOCK],
}

impl<D: BlockDev> Volume<D> {
    pub fn format(dev: D, total_blocks: u32, label: &[u8]) -> Result<Self, MxError> {
        if total_blocks < DATA_FIRST || total_blocks > MAX_BITMAP_BLOCKS {
            return Err(MxError::Invalid);
        }
        let free_blocks = total_blocks - DATA_FIRST;
        let mut label_buf = [0u8; 16];
        let copy = label.len().min(16);
        label_buf[..copy].copy_from_slice(&label[..copy]);

        let sb = Superblock {
            magic: MAGIC,
            version: VERSION,
            block_size: BLOCK as u32,
            total_blocks,
            inode_count: INODE_COUNT,
            bitmap_block: BITMAP_BLOCK,
            inode_table_block: INODE_TABLE_BLOCK,
            inode_table_blocks: INODE_TABLE_BLOCKS,
            data_first: DATA_FIRST,
            root_ino: ROOT_INO,
            mount_count: 0,
            free_blocks,
            label: label_buf,
        };

        let mut vol = Volume {
            dev,
            sb,
            block_buf: [0u8; BLOCK],
        };

        vol.write_superblock()?;

        vol.block_buf.fill(0);
        for b in 0..DATA_FIRST {
            vol.set_bitmap_bit(b, true)?;
        }
        vol.write_bitmap()?;

        for ino in 1..INODE_COUNT {
            let inode = if ino == ROOT_INO {
                Inode {
                    kind: INODE_KIND_DIR,
                    nlink: 1,
                    size: 0,
                    mtime: 0,
                    direct: [0; MAX_DIRECT],
                    indirect: 0,
                }
            } else {
                Inode {
                    kind: 0,
                    nlink: 0,
                    size: 0,
                    mtime: 0,
                    direct: [0; MAX_DIRECT],
                    indirect: 0,
                }
            };
            vol.write_inode(ino, &inode)?;
        }

        Ok(vol)
    }

    pub fn mount(mut dev: D) -> Result<Self, MxError> {
        let mut block_buf = [0u8; BLOCK];
        dev.read_block(0, &mut block_buf)?;
        let sb = Superblock::decode(&block_buf)?;
        if sb.magic != MAGIC {
            return Err(MxError::BadMagic);
        }
        if sb.version != VERSION {
            return Err(MxError::BadVersion);
        }
        if sb.block_size != BLOCK as u32 {
            return Err(MxError::BadVersion);
        }

        let mut vol = Volume {
            dev,
            sb,
            block_buf,
        };
        vol.sb.mount_count += 1;
        vol.write_superblock()?;
        Ok(vol)
    }

    pub fn mount_count(&self) -> u32 {
        self.sb.mount_count
    }

    pub fn free_blocks(&self) -> u32 {
        self.sb.free_blocks
    }

    pub fn total_blocks(&self) -> u32 {
        self.sb.total_blocks
    }

    pub fn inode_count(&self) -> u32 {
        self.sb.inode_count
    }

    pub fn into_device(self) -> D {
        self.dev
    }

    pub fn stat_fs(&mut self) -> Result<FsStat, MxError> {
        Ok(FsStat {
            total_blocks: self.sb.total_blocks,
            free_blocks: self.sb.free_blocks,
            inode_count: self.sb.inode_count,
            mount_count: self.sb.mount_count,
        })
    }

    pub fn list(&mut self, path: &[u8], dst: &mut [u8]) -> Result<usize, MxError> {
        let ino = self.walk(path, false)?;
        let inode = self.read_inode(ino)?;
        if inode.kind != INODE_KIND_DIR {
            return Err(MxError::NotDir);
        }
        self.list_dir(&inode, dst)
    }

    pub fn read_at(
        &mut self,
        path: &[u8],
        offset: u64,
        dst: &mut [u8],
    ) -> Result<usize, MxError> {
        let ino = self.walk(path, false)?;
        let inode = self.read_inode(ino)?;
        if inode.kind == INODE_KIND_DIR {
            return Err(MxError::IsDir);
        }
        let size = inode.size as u64;
        if offset >= size {
            return Ok(0);
        }
        let avail = (size - offset) as usize;
        let to_read = avail.min(dst.len());
        self.read_file_data(&inode, offset, &mut dst[..to_read])?;
        Ok(to_read)
    }

    pub fn write_at(
        &mut self,
        path: &[u8],
        offset: u64,
        data: &[u8],
        flags: u32,
    ) -> Result<usize, MxError> {
        if path.is_empty() || path[0] != b'/' {
            return Err(MxError::Invalid);
        }
        if path == b"/" {
            return Err(MxError::IsDir);
        }

        let (parent_ino, name) = self.walk_parent(path)?;
        let existing = self.lookup_in_dir(parent_ino, name.as_slice())?;

        if existing.is_none() {
            if flags & CREATE == 0 {
                return Err(MxError::NotFound);
            }
            return self.create_file(parent_ino, name.as_slice(), offset, data, flags);
        }

        let ino = existing.unwrap();
        let mut inode = self.read_inode(ino)?;
        if inode.kind == INODE_KIND_DIR {
            return Err(MxError::IsDir);
        }

        let mut write_off = offset;
        if flags & APPEND != 0 {
            write_off = inode.size as u64;
        }
        if flags & TRUNC != 0 {
            self.truncate_inode(&mut inode)?;
        }

        let written = self.write_file_data(&mut inode, write_off, data)?;
        self.write_inode(ino, &inode)?;
        Ok(written)
    }

    pub fn stat(&mut self, path: &[u8]) -> Result<Stat, MxError> {
        let ino = self.walk(path, false)?;
        let inode = self.read_inode(ino)?;
        Ok(Stat {
            kind: inode.kind,
            size: inode.size,
            nlink: inode.nlink,
            mtime: inode.mtime,
            ino,
        })
    }

    pub fn mkdir(&mut self, path: &[u8]) -> Result<(), MxError> {
        if path.is_empty() || path[0] != b'/' || path == b"/" {
            return Err(MxError::Invalid);
        }
        let (parent_ino, name) = self.walk_parent(path)?;
        if !valid_name(name.as_slice()) {
            return Err(MxError::Invalid);
        }
        if self.lookup_in_dir(parent_ino, name.as_slice())?.is_some() {
            return Err(MxError::Exists);
        }

        let child_ino = self.alloc_inode()?;
        let child = Inode {
            kind: INODE_KIND_DIR,
            nlink: 1,
            size: 0,
            mtime: 0,
            direct: [0; MAX_DIRECT],
            indirect: 0,
        };
        self.write_inode(child_ino, &child)?;
        self.add_dir_entry(parent_ino, name.as_slice(), child_ino, INODE_KIND_DIR)?;
        Ok(())
    }

    pub fn unlink(&mut self, path: &[u8]) -> Result<(), MxError> {
        if path.is_empty() || path[0] != b'/' || path == b"/" {
            return Err(MxError::Invalid);
        }
        let (parent_ino, name) = self.walk_parent(path)?;
        let name = name.as_slice();
        let ino = self
            .lookup_in_dir(parent_ino, name)?
            .ok_or(MxError::NotFound)?;

        let inode = self.read_inode(ino)?;
        if inode.kind == INODE_KIND_DIR {
            if !self.dir_is_empty(&inode)? {
                return Err(MxError::NotEmpty);
            }
        }

        self.remove_dir_entry(parent_ino, name)?;
        self.free_inode_blocks(&inode)?;
        let cleared = Inode {
            kind: 0,
            nlink: 0,
            size: 0,
            mtime: 0,
            direct: [0; MAX_DIRECT],
            indirect: 0,
        };
        self.write_inode(ino, &cleared)?;
        Ok(())
    }

    pub fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), MxError> {
        if from.is_empty() || from[0] != b'/' || from == b"/" {
            return Err(MxError::Invalid);
        }
        if to.is_empty() || to[0] != b'/' || to == b"/" {
            return Err(MxError::Invalid);
        }
        let (from_parent, from_name) = self.walk_parent(from)?;
        let from_name = from_name.as_slice();
        let ino = self
            .lookup_in_dir(from_parent, from_name)?
            .ok_or(MxError::NotFound)?;

        let (to_parent, to_name) = self.walk_parent(to)?;
        let to_name = to_name.as_slice();
        if !valid_name(to_name) {
            return Err(MxError::Invalid);
        }
        if self.lookup_in_dir(to_parent, to_name)?.is_some() {
            return Err(MxError::Exists);
        }

        let inode = self.read_inode(ino)?;
        let kind = inode.kind;
        self.remove_dir_entry(from_parent, from_name)?;
        self.add_dir_entry(to_parent, to_name, ino, kind)?;
        Ok(())
    }

    // --- internal ---

    fn write_superblock(&mut self) -> Result<(), MxError> {
        self.sb.encode(&mut self.block_buf);
        self.dev.write_block(0, &mut self.block_buf)
    }

    fn read_bitmap(&mut self) -> Result<(), MxError> {
        self.dev.read_block(self.sb.bitmap_block, &mut self.block_buf)
    }

    fn write_bitmap(&mut self) -> Result<(), MxError> {
        self.dev.write_block(self.sb.bitmap_block, &mut self.block_buf)
    }

    fn set_bitmap_bit(&mut self, block: u32, allocated: bool) -> Result<(), MxError> {
        self.read_bitmap()?;
        let byte = (block / 8) as usize;
        let bit = (block % 8) as u8;
        if byte >= BLOCK {
            return Err(MxError::Invalid);
        }
        if allocated {
            self.block_buf[byte] |= 1 << bit;
        } else {
            self.block_buf[byte] &= !(1 << bit);
        }
        self.write_bitmap()
    }

    fn bitmap_is_set(&mut self, block: u32) -> Result<bool, MxError> {
        self.read_bitmap()?;
        let byte = (block / 8) as usize;
        let bit = (block % 8) as u8;
        if byte >= BLOCK {
            return Ok(false);
        }
        Ok((self.block_buf[byte] >> bit) & 1 == 1)
    }

    fn alloc_data_block(&mut self) -> Result<u32, MxError> {
        if self.sb.free_blocks == 0 {
            return Err(MxError::NoSpace);
        }
        for b in self.sb.data_first..self.sb.total_blocks {
            if !self.bitmap_is_set(b)? {
                self.set_bitmap_bit(b, true)?;
                self.write_bitmap()?;
                self.sb.free_blocks -= 1;
                self.write_superblock()?;
                self.block_buf.fill(0);
                self.dev.write_block(b, &mut self.block_buf)?;
                return Ok(b);
            }
        }
        Err(MxError::NoSpace)
    }

    fn free_data_block(&mut self, block: u32) -> Result<(), MxError> {
        if block < self.sb.data_first {
            return Ok(());
        }
        self.set_bitmap_bit(block, false)?;
        self.write_bitmap()?;
        self.sb.free_blocks += 1;
        self.write_superblock()?;
        Ok(())
    }

    fn alloc_inode(&mut self) -> Result<u32, MxError> {
        for ino in 1..self.sb.inode_count {
            let inode = self.read_inode(ino)?;
            if inode.kind == 0 {
                return Ok(ino);
            }
        }
        Err(MxError::NoSpace)
    }

    fn inode_block_offset(ino: u32) -> (u32, usize) {
        let idx = ino as usize;
        let block = INODE_TABLE_BLOCK + (idx / INODES_PER_BLOCK) as u32;
        let off = (idx % INODES_PER_BLOCK) * INODE_SIZE;
        (block, off)
    }

    fn read_inode(&mut self, ino: u32) -> Result<Inode, MxError> {
        if ino == 0 || ino >= self.sb.inode_count {
            return Err(MxError::Invalid);
        }
        let (block, off) = Self::inode_block_offset(ino);
        self.dev.read_block(block, &mut self.block_buf)?;
        Ok(Inode::decode(&self.block_buf[off..off + INODE_SIZE]))
    }

    fn write_inode(&mut self, ino: u32, inode: &Inode) -> Result<(), MxError> {
        if ino == 0 || ino >= self.sb.inode_count {
            return Err(MxError::Invalid);
        }
        let (block, off) = Self::inode_block_offset(ino);
        self.dev.read_block(block, &mut self.block_buf)?;
        inode.encode(&mut self.block_buf[off..off + INODE_SIZE]);
        self.dev.write_block(block, &mut self.block_buf)
    }

    fn walk(&mut self, path: &[u8], parent_ok: bool) -> Result<u32, MxError> {
        if path.len() > MAX_PATH {
            return Err(MxError::Invalid);
        }
        if path.is_empty() || path == b"/" {
            return Ok(ROOT_INO);
        }
        if path[0] != b'/' {
            return Err(MxError::Invalid);
        }
        let parts = split_path(path)?;
        let mut cur = ROOT_INO;
        for i in 0..parts.len {
            let name = parts.component(path, i);
            let inode = self.read_inode(cur)?;
            if inode.kind != INODE_KIND_DIR {
                return Err(MxError::NotDir);
            }
            if i + 1 == parts.len && parent_ok {
                return Ok(cur);
            }
            let next = self
                .lookup_in_dir(cur, name)?
                .ok_or(MxError::NotFound)?;
            cur = next;
        }
        Ok(cur)
    }

    fn walk_parent(&mut self, path: &[u8]) -> Result<(u32, NameBuf), MxError> {
        if path.len() > MAX_PATH || path.is_empty() || path[0] != b'/' {
            return Err(MxError::Invalid);
        }
        let slash = path
            .iter()
            .rposition(|&b| b == b'/')
            .ok_or(MxError::Invalid)?;
        let (parent_ino, name) = if slash == 0 {
            let name = &path[1..];
            if name.is_empty() || name.len() > MAX_NAME {
                return Err(MxError::Invalid);
            }
            (ROOT_INO, name)
        } else {
            let parent_path = &path[..slash];
            let name = &path[slash + 1..];
            if name.is_empty() || name.len() > MAX_NAME {
                return Err(MxError::Invalid);
            }
            let parent_ino = if parent_path == b"/" {
                ROOT_INO
            } else {
                self.walk(parent_path, false)?
            };
            (parent_ino, name)
        };
        let parent = self.read_inode(parent_ino)?;
        if parent.kind != INODE_KIND_DIR {
            return Err(MxError::NotDir);
        }
        Ok((parent_ino, NameBuf::from_slice(name)))
    }

    fn lookup_in_dir(&mut self, dir_ino: u32, name: &[u8]) -> Result<Option<u32>, MxError> {
        let inode = self.read_inode(dir_ino)?;
        if inode.kind != INODE_KIND_DIR {
            return Err(MxError::NotDir);
        }
        self.scan_dir(&inode, |entry_name, ino, _kind| {
            if entry_name == name {
                return Some(ino);
            }
            None
        })
    }

    fn scan_dir<T>(
        &mut self,
        dir: &Inode,
        mut f: impl FnMut(&[u8], u32, u8) -> Option<T>,
    ) -> Result<Option<T>, MxError> {
        let nbytes = dir.size as usize;
        let nentries = nbytes / DIR_ENTRY_SIZE;
        let mut idx = 0usize;
        while idx < nentries {
            let entry_off = idx * DIR_ENTRY_SIZE;
            let block_idx = entry_off / BLOCK;
            let off_in_block = entry_off % BLOCK;
            if block_idx >= MAX_DIRECT {
                break;
            }
            let bnum = dir.direct[block_idx];
            if bnum == 0 {
                break;
            }
            self.dev.read_block(bnum, &mut self.block_buf)?;
            let entry = DirEntry::decode(&self.block_buf[off_in_block..off_in_block + DIR_ENTRY_SIZE]);
            if entry.ino != 0 {
                let nlen = entry.name_len as usize;
                if nlen > 0 && nlen <= MAX_NAME {
                    let name = &entry.name[..nlen];
                    if let Some(v) = f(name, entry.ino, entry.kind) {
                        return Ok(Some(v));
                    }
                }
            }
            idx += 1;
        }
        Ok(None)
    }

    fn list_dir(&mut self, dir: &Inode, dst: &mut [u8]) -> Result<usize, MxError> {
        let mut written = 0usize;
        let mut first = true;
        let nbytes = dir.size as usize;
        let nentries = nbytes / DIR_ENTRY_SIZE;
        let mut idx = 0usize;
        while idx < nentries {
            let entry_off = idx * DIR_ENTRY_SIZE;
            let block_idx = entry_off / BLOCK;
            let off_in_block = entry_off % BLOCK;
            if block_idx >= MAX_DIRECT {
                break;
            }
            let bnum = dir.direct[block_idx];
            if bnum == 0 {
                break;
            }
            self.dev.read_block(bnum, &mut self.block_buf)?;
            let entry = DirEntry::decode(&self.block_buf[off_in_block..off_in_block + DIR_ENTRY_SIZE]);
            if entry.ino != 0 {
                let nlen = entry.name_len as usize;
                if nlen > 0 {
                    if !first {
                        if written < dst.len() {
                            dst[written] = b' ';
                            written += 1;
                        } else {
                            return Ok(written);
                        }
                    }
                    first = false;
                    let name = &entry.name[..nlen];
                    for &byte in name {
                        if written < dst.len() {
                            dst[written] = byte;
                            written += 1;
                        } else {
                            return Ok(written);
                        }
                    }
                }
            }
            idx += 1;
        }
        Ok(written)
    }

    fn dir_is_empty(&mut self, dir: &Inode) -> Result<bool, MxError> {
        let found = self.scan_dir(dir, |_name, ino, _kind| {
            if ino != 0 {
                return Some(true);
            }
            None
        })?;
        Ok(found.is_none())
    }

    fn add_dir_entry(
        &mut self,
        parent_ino: u32,
        name: &[u8],
        child_ino: u32,
        kind: u16,
    ) -> Result<(), MxError> {
        let mut parent = self.read_inode(parent_ino)?;
        if parent.kind != INODE_KIND_DIR {
            return Err(MxError::NotDir);
        }

        let slot = self.find_free_dir_slot(&mut parent)?;
        let mut entry = DirEntry {
            ino: child_ino,
            kind: kind as u8,
            name_len: name.len() as u8,
            name: [0; MAX_NAME],
        };
        entry.name[..name.len()].copy_from_slice(name);

        let entry_off = slot * DIR_ENTRY_SIZE;
        let block_idx = entry_off / BLOCK;
        let off_in_block = entry_off % BLOCK;

        if block_idx >= MAX_DIRECT {
            return Err(MxError::NoSpace);
        }

        let bnum = if parent.direct[block_idx] == 0 {
            let blk = self.alloc_data_block()?;
            parent.direct[block_idx] = blk;
            blk
        } else {
            parent.direct[block_idx]
        };

        self.dev.read_block(bnum, &mut self.block_buf)?;
        entry.encode(&mut self.block_buf[off_in_block..off_in_block + DIR_ENTRY_SIZE]);
        self.dev.write_block(bnum, &mut self.block_buf)?;

        let needed = (slot + 1) * DIR_ENTRY_SIZE;
        if needed as u32 > parent.size {
            parent.size = needed as u32;
        }
        self.write_inode(parent_ino, &parent)?;
        Ok(())
    }

    fn find_free_dir_slot(&mut self, dir: &Inode) -> Result<usize, MxError> {
        let nentries = dir.size as usize / DIR_ENTRY_SIZE;
        for idx in 0..nentries {
            let entry_off = idx * DIR_ENTRY_SIZE;
            let block_idx = entry_off / BLOCK;
            let off_in_block = entry_off % BLOCK;
            if block_idx >= MAX_DIRECT {
                break;
            }
            let bnum = dir.direct[block_idx];
            if bnum == 0 {
                continue;
            }
            self.dev.read_block(bnum, &mut self.block_buf)?;
            let entry = DirEntry::decode(&self.block_buf[off_in_block..off_in_block + DIR_ENTRY_SIZE]);
            if entry.ino == 0 {
                return Ok(idx);
            }
        }
        let new_slot = nentries;
        if new_slot >= MAX_DIRECT * ENTRIES_PER_BLOCK {
            return Err(MxError::NoSpace);
        }
        Ok(new_slot)
    }

    fn remove_dir_entry(&mut self, parent_ino: u32, name: &[u8]) -> Result<(), MxError> {
        let parent = self.read_inode(parent_ino)?;
        let nentries = parent.size as usize / DIR_ENTRY_SIZE;
        for idx in 0..nentries {
            let entry_off = idx * DIR_ENTRY_SIZE;
            let block_idx = entry_off / BLOCK;
            let off_in_block = entry_off % BLOCK;
            if block_idx >= MAX_DIRECT {
                break;
            }
            let bnum = parent.direct[block_idx];
            if bnum == 0 {
                continue;
            }
            self.dev.read_block(bnum, &mut self.block_buf)?;
            let entry = DirEntry::decode(&self.block_buf[off_in_block..off_in_block + DIR_ENTRY_SIZE]);
            if entry.ino != 0 {
                let nlen = entry.name_len as usize;
                if nlen > 0 && &entry.name[..nlen] == name {
                    let tomb = DirEntry {
                        ino: 0,
                        kind: 0,
                        name_len: 0,
                        name: [0; MAX_NAME],
                    };
                    tomb.encode(&mut self.block_buf[off_in_block..off_in_block + DIR_ENTRY_SIZE]);
                    self.dev.write_block(bnum, &mut self.block_buf)?;
                    return Ok(());
                }
            }
        }
        Err(MxError::NotFound)
    }

    fn create_file(
        &mut self,
        parent_ino: u32,
        name: &[u8],
        offset: u64,
        data: &[u8],
        flags: u32,
    ) -> Result<usize, MxError> {
        if !valid_name(name) {
            return Err(MxError::Invalid);
        }
        let ino = self.alloc_inode()?;
        let mut inode = Inode {
            kind: INODE_KIND_FILE,
            nlink: 1,
            size: 0,
            mtime: 0,
            direct: [0; MAX_DIRECT],
            indirect: 0,
        };

        let mut write_off = offset;
        if flags & APPEND != 0 {
            write_off = 0;
        }
        if flags & TRUNC != 0 {
            inode.size = 0;
        }

        let written = if data.is_empty() {
            0
        } else {
            self.write_file_data_ordered(&mut inode, write_off, data)?
        };

        self.write_inode(ino, &inode)?;
        self.add_dir_entry(parent_ino, name, ino, INODE_KIND_FILE)?;
        Ok(written)
    }

    /// Write order for new files: data, inode, bitmap, dentry.
    fn write_file_data_ordered(
        &mut self,
        inode: &mut Inode,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, MxError> {
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset + data.len() as u64;
        let new_size = end.max(inode.size as u64) as u32;
        let written = self.write_range(inode, offset, data)?;
        inode.size = new_size;
        Ok(written)
    }

    fn write_file_data(
        &mut self,
        inode: &mut Inode,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, MxError> {
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset + data.len() as u64;
        let new_size = end.max(inode.size as u64) as u32;
        let written = self.write_range(inode, offset, data)?;
        inode.size = new_size;
        Ok(written)
    }

    fn write_range(&mut self, inode: &mut Inode, offset: u64, data: &[u8]) -> Result<usize, MxError> {
        let mut pos = 0usize;
        while pos < data.len() {
            let file_off = offset + pos as u64;
            let lbn = (file_off / BLOCK as u64) as usize;
            let off_in_block = (file_off % BLOCK as u64) as usize;
            let chunk = (BLOCK - off_in_block).min(data.len() - pos);

            let pbn = self.ensure_file_block(inode, lbn)?;
            self.dev.read_block(pbn, &mut self.block_buf)?;
            self.block_buf[off_in_block..off_in_block + chunk]
                .copy_from_slice(&data[pos..pos + chunk]);
            self.dev.write_block(pbn, &mut self.block_buf)?;
            pos += chunk;
        }
        Ok(data.len())
    }

    fn ensure_file_block(&mut self, inode: &mut Inode, lbn: usize) -> Result<u32, MxError> {
        if lbn < MAX_DIRECT {
            if inode.direct[lbn] == 0 {
                inode.direct[lbn] = self.alloc_data_block()?;
            }
            return Ok(inode.direct[lbn]);
        }
        let ind_idx = lbn - MAX_DIRECT;
        if ind_idx >= INDIRECT_PTRS {
            return Err(MxError::NoSpace);
        }
        if inode.indirect == 0 {
            inode.indirect = self.alloc_data_block()?;
            self.block_buf.fill(0);
            self.dev.write_block(inode.indirect, &mut self.block_buf)?;
        }
        self.dev.read_block(inode.indirect, &mut self.block_buf)?;
        let off = ind_idx * 4;
        let existing = read_u32(&self.block_buf, off);
        if existing == 0 {
            let blk = self.alloc_data_block()?;
            self.dev.read_block(inode.indirect, &mut self.block_buf)?;
            write_u32(&mut self.block_buf, off, blk);
            self.dev.write_block(inode.indirect, &mut self.block_buf)?;
            Ok(blk)
        } else {
            Ok(existing)
        }
    }

    fn file_block_num(&mut self, inode: &Inode, lbn: usize) -> Result<Option<u32>, MxError> {
        if lbn < MAX_DIRECT {
            let b = inode.direct[lbn];
            return Ok(if b == 0 { None } else { Some(b) });
        }
        let ind_idx = lbn - MAX_DIRECT;
        if ind_idx >= INDIRECT_PTRS || inode.indirect == 0 {
            return Ok(None);
        }
        self.dev.read_block(inode.indirect, &mut self.block_buf)?;
        let b = read_u32(&self.block_buf, ind_idx * 4);
        Ok(if b == 0 { None } else { Some(b) })
    }

    fn read_file_data(&mut self, inode: &Inode, offset: u64, dst: &mut [u8]) -> Result<(), MxError> {
        let mut pos = 0usize;
        while pos < dst.len() {
            let file_off = offset + pos as u64;
            if file_off >= inode.size as u64 {
                break;
            }
            let lbn = (file_off / BLOCK as u64) as usize;
            let off_in_block = (file_off % BLOCK as u64) as usize;
            let pbn = self
                .file_block_num(inode, lbn)?
                .ok_or(MxError::Invalid)?;
            self.dev.read_block(pbn, &mut self.block_buf)?;
            let chunk = (BLOCK - off_in_block).min(dst.len() - pos);
            let max_chunk = (inode.size as u64 - file_off) as usize;
            let chunk = chunk.min(max_chunk);
            dst[pos..pos + chunk]
                .copy_from_slice(&self.block_buf[off_in_block..off_in_block + chunk]);
            pos += chunk;
        }
        Ok(())
    }

    fn truncate_inode(&mut self, inode: &mut Inode) -> Result<(), MxError> {
        self.free_inode_blocks(inode)?;
        inode.size = 0;
        inode.direct = [0; MAX_DIRECT];
        inode.indirect = 0;
        Ok(())
    }

    fn free_inode_blocks(&mut self, inode: &Inode) -> Result<(), MxError> {
        let nblocks = blocks_for_size(inode.size);
        for lbn in 0..nblocks {
            if let Some(pbn) = self.file_block_num(inode, lbn)? {
                self.free_data_block(pbn)?;
            }
        }
        if inode.indirect != 0 {
            self.free_data_block(inode.indirect)?;
        }
        Ok(())
    }
}

fn blocks_for_size(size: u32) -> usize {
    if size == 0 {
        0
    } else {
        ((size as usize) + BLOCK - 1) / BLOCK
    }
}

struct DirEntry {
    ino: u32,
    kind: u8,
    name_len: u8,
    name: [u8; MAX_NAME],
}

impl DirEntry {
    fn decode(bytes: &[u8]) -> Self {
        Self {
            ino: read_u32(bytes, 0),
            kind: bytes[4],
            name_len: bytes[5],
            name: {
                let mut n = [0u8; MAX_NAME];
                n.copy_from_slice(&bytes[8..8 + MAX_NAME]);
                n
            },
        }
    }

    fn encode(&self, bytes: &mut [u8]) {
        write_u32(bytes, 0, self.ino);
        bytes[4] = self.kind;
        bytes[5] = self.name_len;
        bytes[6] = 0;
        bytes[7] = 0;
        bytes[8..8 + MAX_NAME].copy_from_slice(&self.name);
    }
}

impl Superblock {
    fn decode(buf: &[u8; BLOCK]) -> Result<Self, MxError> {
        let magic = read_u32(buf, 0);
        if magic != MAGIC {
            return Err(MxError::BadMagic);
        }
        let mut label = [0u8; 16];
        label.copy_from_slice(&buf[48..64]);
        Ok(Self {
            magic,
            version: read_u32(buf, 4),
            block_size: read_u32(buf, 8),
            total_blocks: read_u32(buf, 12),
            inode_count: read_u32(buf, 16),
            bitmap_block: read_u32(buf, 20),
            inode_table_block: read_u32(buf, 24),
            inode_table_blocks: read_u32(buf, 28),
            data_first: read_u32(buf, 32),
            root_ino: read_u32(buf, 36),
            mount_count: read_u32(buf, 40),
            free_blocks: read_u32(buf, 44),
            label,
        })
    }

    fn encode(&self, buf: &mut [u8; BLOCK]) {
        buf.fill(0);
        write_u32(buf, 0, self.magic);
        write_u32(buf, 4, self.version);
        write_u32(buf, 8, self.block_size);
        write_u32(buf, 12, self.total_blocks);
        write_u32(buf, 16, self.inode_count);
        write_u32(buf, 20, self.bitmap_block);
        write_u32(buf, 24, self.inode_table_block);
        write_u32(buf, 28, self.inode_table_blocks);
        write_u32(buf, 32, self.data_first);
        write_u32(buf, 36, self.root_ino);
        write_u32(buf, 40, self.mount_count);
        write_u32(buf, 44, self.free_blocks);
        buf[48..64].copy_from_slice(&self.label);
    }
}

impl Inode {
    fn decode(bytes: &[u8]) -> Self {
        let mut direct = [0u32; MAX_DIRECT];
        for i in 0..MAX_DIRECT {
            direct[i] = read_u32(bytes, 16 + i * 4);
        }
        Self {
            kind: read_u16(bytes, 0),
            nlink: read_u16(bytes, 2),
            size: read_u32(bytes, 4),
            mtime: read_u64(bytes, 8),
            direct,
            indirect: read_u32(bytes, 48),
        }
    }

    fn encode(&self, bytes: &mut [u8]) {
        bytes.fill(0);
        write_u16(bytes, 0, self.kind);
        write_u16(bytes, 2, self.nlink);
        write_u32(bytes, 4, self.size);
        write_u64(bytes, 8, self.mtime);
        for i in 0..MAX_DIRECT {
            write_u32(bytes, 16 + i * 4, self.direct[i]);
        }
        write_u32(bytes, 48, self.indirect);
    }
}

fn read_u16(bytes: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([bytes[off], bytes[off + 1]])
}

fn read_u32(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

fn read_u64(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        bytes[off],
        bytes[off + 1],
        bytes[off + 2],
        bytes[off + 3],
        bytes[off + 4],
        bytes[off + 5],
        bytes[off + 6],
        bytes[off + 7],
    ])
}

fn write_u16(bytes: &mut [u8], off: usize, value: u16) {
    bytes[off..off + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], off: usize, value: u32) {
    bytes[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], off: usize, value: u64) {
    bytes[off..off + 8].copy_from_slice(&value.to_le_bytes());
}

fn valid_name(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > MAX_NAME {
        return false;
    }
    if name == b"." || name == b".." {
        return false;
    }
    !name.contains(&b'/')
}

struct NameBuf {
    bytes: [u8; MAX_NAME],
    len: u8,
}

impl NameBuf {
    fn from_slice(name: &[u8]) -> Self {
        let mut bytes = [0u8; MAX_NAME];
        bytes[..name.len()].copy_from_slice(name);
        Self {
            bytes,
            len: name.len() as u8,
        }
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

struct PathParts {
    spans: [(usize, usize); MAX_DEPTH],
    len: usize,
}

impl PathParts {
    fn component<'a>(&self, path: &'a [u8], index: usize) -> &'a [u8] {
        let (start, end) = self.spans[index];
        &path[start..end]
    }
}

fn split_path(path: &[u8]) -> Result<PathParts, MxError> {
    if path.len() > MAX_PATH {
        return Err(MxError::Invalid);
    }
    let mut parts = PathParts {
        spans: [(0, 0); MAX_DEPTH],
        len: 0,
    };
    let mut i = 0usize;
    if path.starts_with(b"/") {
        i = 1;
    }
    while i < path.len() {
        if path[i] == b'/' {
            i += 1;
            continue;
        }
        let start = i;
        while i < path.len() && path[i] != b'/' {
            i += 1;
        }
        if start == i {
            continue;
        }
        let comp_len = i - start;
        if comp_len > MAX_NAME {
            return Err(MxError::Invalid);
        }
        let comp = &path[start..i];
        if !valid_name(comp) {
            return Err(MxError::Invalid);
        }
        if parts.len >= MAX_DEPTH {
            return Err(MxError::Invalid);
        }
        parts.spans[parts.len] = (start, i);
        parts.len += 1;
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    struct MemDisk {
        blocks: Vec<[u8; BLOCK]>,
    }

    impl MemDisk {
        fn new(nblocks: u32) -> Self {
            let n = nblocks as usize;
            Self {
                blocks: std::iter::repeat([0u8; BLOCK]).take(n).collect(),
            }
        }
    }

    impl BlockDev for MemDisk {
        fn read_block(&mut self, block: u32, buf: &mut [u8; BLOCK]) -> Result<(), MxError> {
            let b = block as usize;
            if b >= self.blocks.len() {
                return Err(MxError::Io);
            }
            buf.copy_from_slice(&self.blocks[b]);
            Ok(())
        }

        fn write_block(&mut self, block: u32, buf: &mut [u8; BLOCK]) -> Result<(), MxError> {
            let b = block as usize;
            if b >= self.blocks.len() {
                return Err(MxError::Io);
            }
            self.blocks[b].copy_from_slice(buf);
            Ok(())
        }
    }

    const ALPHA_BLOCKS: u32 = 16384;

    #[test]
    fn format_mount_round_trip() {
        let disk = MemDisk::new(ALPHA_BLOCKS);
        let vol = Volume::format(disk, ALPHA_BLOCKS, b"test").unwrap();
        assert_eq!(vol.mount_count(), 0);
        assert_eq!(vol.free_blocks(), ALPHA_BLOCKS - DATA_FIRST);

        let disk = vol.into_device();
        let mut vol2 = Volume::mount(disk).unwrap();
        assert_eq!(vol2.mount_count(), 1);
        let mut names = [0u8; 256];
        let n = vol2.list(b"/", &mut names).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn mkdir_write_read_note() {
        let disk = MemDisk::new(ALPHA_BLOCKS);
        let mut vol = Volume::format(disk, ALPHA_BLOCKS, b"").unwrap();
        vol.mkdir(b"/home").unwrap();
        let body = b"meuxe-phase3";
        vol.write_at(b"/home/note", 0, body, CREATE).unwrap();
        let mut buf = [0u8; 32];
        let n = vol.read_at(b"/home/note", 0, &mut buf).unwrap();
        assert_eq!(n, body.len());
        assert_eq!(&buf[..n], body);
    }

    #[test]
    fn unlink_and_remount() {
        let disk = MemDisk::new(ALPHA_BLOCKS);
        let mut vol = Volume::format(disk, ALPHA_BLOCKS, b"").unwrap();
        vol.mkdir(b"/home").unwrap();
        vol.write_at(b"/home/note", 0, b"meuxe-phase3", CREATE).unwrap();
        vol.write_at(b"/home/tmp", 0, b"x", CREATE).unwrap();
        vol.unlink(b"/home/tmp").unwrap();
        assert!(matches!(
            vol.stat(b"/home/tmp"),
            Err(MxError::NotFound)
        ));

        let disk = vol.into_device();
        let mut vol2 = Volume::mount(disk).unwrap();
        assert!(matches!(
            vol2.stat(b"/home/tmp"),
            Err(MxError::NotFound)
        ));
        let mut buf = [0u8; 32];
        let n = vol2.read_at(b"/home/note", 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"meuxe-phase3");
    }

    #[test]
    fn rename_across_dirs() {
        let disk = MemDisk::new(ALPHA_BLOCKS);
        let mut vol = Volume::format(disk, ALPHA_BLOCKS, b"").unwrap();
        vol.mkdir(b"/tmp").unwrap();
        vol.mkdir(b"/home").unwrap();
        vol.write_at(b"/tmp/a", 0, b"data", CREATE).unwrap();
        vol.rename(b"/tmp/a", b"/home/b").unwrap();
        assert!(matches!(vol.stat(b"/tmp/a"), Err(MxError::NotFound)));
        let mut buf = [0u8; 8];
        let n = vol.read_at(b"/home/b", 0, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"data");
    }

    #[test]
    fn unlink_empty_and_nonempty_dir() {
        let disk = MemDisk::new(ALPHA_BLOCKS);
        let mut vol = Volume::format(disk, ALPHA_BLOCKS, b"").unwrap();
        vol.mkdir(b"/empty").unwrap();
        vol.unlink(b"/empty").unwrap();
        vol.mkdir(b"/full").unwrap();
        vol.write_at(b"/full/f", 0, b"x", CREATE).unwrap();
        assert!(matches!(vol.unlink(b"/full"), Err(MxError::NotEmpty)));
    }

    #[test]
    fn nospace_small_disk() {
        let disk = MemDisk::new(64);
        let mut vol = Volume::format(disk, 64, b"").unwrap();
        let mut i = 0u32;
        loop {
            let path = std::format!("/f{i}");
            match vol.write_at(path.as_bytes(), 0, b"x", CREATE) {
                Ok(_) => i += 1,
                Err(MxError::NoSpace) => break,
                Err(e) => panic!("unexpected {e:?}"),
            }
        }
        assert!(i > 0);
    }

    #[test]
    fn large_file_indirect() {
        let disk = MemDisk::new(ALPHA_BLOCKS);
        let mut vol = Volume::format(disk, ALPHA_BLOCKS, b"").unwrap();
        let data = [0xABu8; 40 * 1024];
        vol.write_at(b"/big", 0, &data, CREATE).unwrap();
        let mut buf = [0u8; 40 * 1024];
        let n = vol.read_at(b"/big", 0, &mut buf).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(buf, data);
    }
}
