use std::fs::File;
use std::io::{Result, Seek, SeekFrom};

use vm_memory::{ReadVolatile, VolatileSlice, WriteVolatile};

pub trait FileSetLen {
    fn set_len(&self, len: u64) -> Result<()>;
}

impl FileSetLen for File {
    fn set_len(&self, len: u64) -> Result<()> {
        File::set_len(self, len)
    }
}

pub trait FileReadWriteVolatile {
    fn read_volatile(&mut self, slice: VolatileSlice) -> Result<usize>;
    fn write_volatile(&mut self, slice: VolatileSlice) -> Result<usize>;

    fn read_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
        if let Some(&slice) = bufs.iter().find(|b| !b.is_empty()) {
            self.read_volatile(slice)
        } else {
            Ok(0)
        }
    }

    fn write_vectored_volatile(&mut self, bufs: &[VolatileSlice]) -> Result<usize> {
        if let Some(&slice) = bufs.iter().find(|b| !b.is_empty()) {
            self.write_volatile(slice)
        } else {
            Ok(0)
        }
    }
}

pub trait FileReadWriteAtVolatile {
    fn read_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize>;
    fn write_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize>;

    fn read_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        if let Some(&slice) = bufs.first() {
            self.read_at_volatile(slice, offset)
        } else {
            Ok(0)
        }
    }

    fn write_vectored_at_volatile(&self, bufs: &[VolatileSlice], offset: u64) -> Result<usize> {
        if let Some(&slice) = bufs.first() {
            self.write_at_volatile(slice, offset)
        } else {
            Ok(0)
        }
    }
}

impl<T: FileReadWriteVolatile + ?Sized> FileReadWriteVolatile for &mut T {
    fn read_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
        (**self).read_volatile(slice)
    }

    fn write_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
        (**self).write_volatile(slice)
    }
}

impl FileReadWriteVolatile for File {
    fn read_volatile(&mut self, mut slice: VolatileSlice) -> Result<usize> {
        ReadVolatile::read_volatile(self, &mut slice).map_err(|e| match e {
            vm_memory::VolatileMemoryError::IOError(err) => err,
            _ => std::io::Error::from(std::io::ErrorKind::Other),
        })
    }

    fn write_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
        WriteVolatile::write_volatile(self, &slice).map_err(|e| match e {
            vm_memory::VolatileMemoryError::IOError(err) => err,
            _ => std::io::Error::from(std::io::ErrorKind::Other),
        })
    }
}

impl FileReadWriteAtVolatile for File {
    fn read_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
        let mut cloned = self.try_clone()?;
        cloned.seek(SeekFrom::Start(offset))?;
        FileReadWriteVolatile::read_volatile(&mut cloned, slice)
    }

    fn write_at_volatile(&self, slice: VolatileSlice, offset: u64) -> Result<usize> {
        let mut cloned = self.try_clone()?;
        cloned.seek(SeekFrom::Start(offset))?;
        FileReadWriteVolatile::write_volatile(&mut cloned, slice)
    }
}
