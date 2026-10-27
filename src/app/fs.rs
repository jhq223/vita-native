//! Owned SDK file handles and atomic no-replace publication primitives.
use super::sdk;
use std::{
    ffi::CString,
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::ffi::OsStrExt,
    path::Path,
    prelude::v1::*,
    time::{Duration, SystemTime},
};

fn path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}
fn checked(code: i32) -> io::Result<i32> {
    if code >= 0 {
        return Ok(code);
    }
    let kind = match code as u32 {
        0x80010002 => io::ErrorKind::NotFound,
        0x80010011 => io::ErrorKind::AlreadyExists,
        0x8001000d => io::ErrorKind::PermissionDenied,
        0x8001001c => io::ErrorKind::StorageFull,
        0x80010004 => io::ErrorKind::Interrupted,
        _ => io::ErrorKind::Other,
    };
    Err(io::Error::new(
        kind,
        super::Error {
            operation: "Vita file IO",
            code,
        },
    ))
}
/// SDK file information with an unsigned 64-bit length and original timestamp.
#[derive(Clone, Debug)]
pub struct Metadata(sdk::SceIoStat);
impl Metadata {
    fn from_stat(stat: sdk::SceIoStat) -> io::Result<Self> {
        if stat.st_size < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "negative native file size",
            ));
        }
        Ok(Self(stat))
    }
    /// File length without the 32-bit newlib off_t truncation.
    pub fn len(&self) -> u64 {
        self.0.st_size as u64
    }
    /// Whether the file has no data bytes.
    pub fn is_empty(&self) -> bool {
        self.0.st_size == 0
    }
    /// Whether the native mode identifies a regular file.
    pub fn is_file(&self) -> bool {
        (self.0.st_mode as u32) & sdk::SCE_S_IFMT == sdk::SCE_S_IFREG
    }
    /// Modification time converted from the SDK's calendar without newlib time_t.
    pub fn modified(&self) -> io::Result<SystemTime> {
        let mut tick = sdk::SceRtcTick { tick: 0 };
        // SAFETY: retained SDK calendar and exclusive initialized tick output.
        checked(unsafe { sdk::sceRtcGetTick(&self.0.st_mtime, &mut tick) })?;
        const EPOCH: u64 = 62_135_596_800_000_000;
        let time = if tick.tick >= EPOCH {
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_micros(tick.tick - EPOCH))
        } else {
            SystemTime::UNIX_EPOCH.checked_sub(Duration::from_micros(EPOCH - tick.tick))
        };
        time.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "native file timestamp overflow")
        })
    }
}
/// Query file information without narrowing its length through newlib stat.
pub fn metadata(pathname: &Path) -> io::Result<Metadata> {
    let name = path(pathname)?;
    let mut stat = unsafe { std::mem::zeroed() };
    // SAFETY: stable NUL-terminated path and exclusive initialized SDK output.
    checked(unsafe { sdk::sceIoGetstat(name.as_ptr(), &mut stat) })?;
    Metadata::from_stat(stat)
}
/// Owned synchronous SDK file descriptor with checked close and file sync.
pub struct File {
    fd: i32,
}
impl File {
    /// Open an existing file for 64-bit read and seek operations.
    pub fn open(pathname: &Path) -> io::Result<Self> {
        let name = path(pathname)?;
        // SAFETY: stable NUL-terminated path; only the returned owner retains the fd.
        let fd = checked(unsafe { sdk::sceIoOpen(name.as_ptr(), sdk::SCE_O_RDONLY as i32, 0) })?;
        Ok(Self { fd })
    }
    /// Query the opened file's actual SDK length, mode and modification time.
    pub fn metadata(&self) -> io::Result<Metadata> {
        let mut stat = unsafe { std::mem::zeroed() };
        // SAFETY: live descriptor and exclusive initialized SDK output.
        checked(unsafe { sdk::sceIoGetstatByFd(self.fd, &mut stat) })?;
        Metadata::from_stat(stat)
    }
    /// Create a new write-only file, preserving an existing destination.
    pub fn create_new(pathname: &Path) -> io::Result<Self> {
        let name = path(pathname)?;
        // SAFETY: path is NUL-terminated and borrowed only during this call.
        let fd = checked(unsafe {
            sdk::sceIoOpen(
                name.as_ptr(),
                (sdk::SCE_O_WRONLY | sdk::SCE_O_CREAT | sdk::SCE_O_EXCL) as i32,
                0o666,
            )
        })?;
        Ok(Self { fd })
    }
    /// Request synchronous file-data flush through sceIoSyncByFd.
    pub fn sync(&self) -> io::Result<()> {
        // SAFETY: owned file descriptor, synchronous completion before publication.
        checked(unsafe { sdk::sceIoSyncByFd(self.fd, 0) }).map(|_| ())
    }
    /// Close this descriptor and report the original SDK error on failure.
    pub fn close(mut self) -> io::Result<()> {
        // SAFETY: closes the sole native owner; a failed close leaves it owned.
        checked(unsafe { sdk::sceIoClose(self.fd) })?;
        self.fd = -1;
        Ok(())
    }
}
impl Write for File {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = bytes.len().min(i32::MAX as usize) as u32;
        // SAFETY: valid input bytes, no pointer retained by synchronous IO.
        checked(unsafe { sdk::sceIoWrite(self.fd, bytes.as_ptr().cast(), count) })
            .map(|n| n as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sync()
    }
}
impl Read for File {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = bytes.len().min(i32::MAX as usize) as u32;
        // SAFETY: exclusive writable bytes and a synchronous SDK operation.
        checked(unsafe { sdk::sceIoRead(self.fd, bytes.as_mut_ptr().cast(), count) })
            .map(|n| n as usize)
    }
}
impl Seek for File {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let (offset, whence) = match from {
            SeekFrom::Start(n) => (
                i64::try_from(n).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "native file offset exceeds i64",
                    )
                })?,
                sdk::SCE_SEEK_SET,
            ),
            SeekFrom::Current(n) => (n, sdk::SCE_SEEK_CUR),
            SeekFrom::End(n) => (n, sdk::SCE_SEEK_END),
        };
        // SAFETY: live descriptor and a signed 64-bit SDK offset, without narrowing.
        let result = unsafe { sdk::sceIoLseek(self.fd, offset, whence as i32) };
        if result < 0 {
            checked(result as i32).map(|n| n as u64)
        } else {
            Ok(result as u64)
        }
    }
}
impl Drop for File {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe {
                sdk::sceIoClose(self.fd);
            }
        }
    }
}
/// Atomic same-filesystem move to a vacant destination. Unlike newlib rename,
/// this preserves an existing destination and reports AlreadyExists.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let from = path(from)?;
    let to = path(to)?;
    // SAFETY: two stable NUL-terminated paths; no borrowed pointer is retained.
    checked(unsafe { sdk::sceIoRename(from.as_ptr(), to.as_ptr()) }).map(|_| ())
}

// VitaSDK newlib supplies the reentrant syscall but omits its POSIX wrapper.
// Delegate to that syscall with the current thread's reentrancy state, preserving
// its ENOSYS error for unsupported symbolic links and std's ordinary error path.
unsafe extern "C" {
    fn __getreent() -> *mut std::ffi::c_void;
    fn _readlink_r(
        reentrant: *mut std::ffi::c_void,
        path: *const std::ffi::c_char,
        output: *mut std::ffi::c_char,
        length: usize,
    ) -> isize;
}
#[unsafe(export_name = "readlink")]
unsafe extern "C" fn readlink_compat(
    path: *const std::ffi::c_char,
    output: *mut std::ffi::c_char,
    length: usize,
) -> isize {
    // SAFETY: caller supplies the POSIX readlink ABI; newlib owns the live,
    // thread-local reentrancy record, and the syscall retains no pointer.
    unsafe { _readlink_r(__getreent(), path, output, length) }
}
