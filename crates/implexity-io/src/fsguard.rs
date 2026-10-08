// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStat {
    pub kind: FileKind,
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u64,
    pub size: u64,
    pub mtime: (i64, i64),
    pub ctime: (i64, i64),
    pub owned: bool,
    pub owner_only: bool,
}

impl FileStat {
    #[must_use]
    pub fn is_file(&self) -> bool {
        self.kind == FileKind::Regular
    }

    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Directory
    }

    #[must_use]
    pub fn is_symlink(&self) -> bool {
        self.kind == FileKind::Symlink
    }

    #[must_use]
    pub fn dev_ino(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }

    #[must_use]
    pub fn same_object(&self, other: &FileStat) -> bool {
        self.dev_ino() == other.dev_ino()
    }

    #[must_use]
    pub fn data_identity(&self) -> (u64, u64, u32, u64, u64, (i64, i64)) {
        (self.dev, self.ino, self.mode, self.nlink, self.size, self.mtime)
    }

    #[must_use]
    pub fn is_owned_single_regular(&self) -> bool {
        self.is_file() && self.nlink == 1 && self.owned
    }
}


pub fn stat_file(file: &File) -> io::Result<FileStat> {
    imp::stat_file(file)
}


pub fn stat_nofollow(path: &Path) -> io::Result<FileStat> {
    imp::stat_nofollow(path)
}


pub fn open_nofollow(path: &Path) -> io::Result<File> {
    imp::open_nofollow(path)
}


pub fn create_new_nofollow(path: &Path, unix_mode: u32) -> io::Result<File> {
    imp::create_new_nofollow(path, unix_mode)
}


pub fn open_or_create_nofollow(path: &Path, unix_mode: u32) -> io::Result<File> {
    imp::open_or_create_nofollow(path, unix_mode)
}


pub fn create_dir_owner_only(path: &Path) -> io::Result<()> {
    imp::create_dir_owner_only(path)
}


pub fn create_dir_all_owner_only(path: &Path) -> io::Result<()> {
    imp::create_dir_all_owner_only(path)
}


pub fn set_owner_only(path: &Path, directory: bool) -> io::Result<()> {
    imp::set_owner_only(path, directory)
}


pub fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    imp::read_at(file, buffer, offset)
}


pub fn fsync_dir(path: &Path) -> io::Result<()> {
    imp::fsync_dir(path)
}

#[must_use]
pub fn monotonic_ns() -> i64 {
    imp::monotonic_ns()
}

#[derive(Debug)]
pub struct Dir {
    inner: imp::Dir,
}

impl Dir {

    pub fn open(path: &Path) -> io::Result<Dir> {
        imp::Dir::open(path).map(|inner| Dir { inner })
    }


    pub fn stat(&self) -> io::Result<FileStat> {
        self.inner.stat()
    }


    pub fn stat_at(&self, name: &str) -> io::Result<FileStat> {
        check_name(name)?;
        self.inner.stat_at(name)
    }


    pub fn open_dir_at(&self, name: &str) -> io::Result<Dir> {
        check_name(name)?;
        self.inner.open_dir_at(name).map(|inner| Dir { inner })
    }


    pub fn open_file_at(&self, name: &str) -> io::Result<File> {
        check_name(name)?;
        self.inner.open_file_at(name)
    }


    pub fn entries(&self) -> io::Result<BTreeSet<String>> {
        self.inner.entries()
    }


    pub fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }
}

fn check_name(name: &str) -> io::Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\0'])
        || (cfg!(windows) && name.contains(['\\', ':']));
    if bad {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsafe directory entry name"));
    }
    Ok(())
}

#[cfg(unix)]
mod imp {
    use std::collections::BTreeSet;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    use rustix::fs::{AtFlags, FileType, Mode, OFlags};

    use super::{FileKind, FileStat};

    fn kind_of(mode: u32) -> FileKind {
        match FileType::from_raw_mode(mode as _) {
            FileType::RegularFile => FileKind::Regular,
            FileType::Directory => FileKind::Directory,
            FileType::Symlink => FileKind::Symlink,
            _ => FileKind::Other,
        }
    }

    fn uid() -> u32 {
        rustix::process::getuid().as_raw()
    }

    fn grants_group_or_other(mode: u32) -> bool {
        mode & 0o077 != 0
    }

    fn from_metadata(m: &std::fs::Metadata) -> FileStat {
        FileStat {
            kind: kind_of(m.mode()),
            dev: m.dev(),
            ino: m.ino(),
            mode: m.mode(),
            nlink: m.nlink(),
            size: m.size(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
            owned: m.uid() == uid(),
            owner_only: !grants_group_or_other(m.mode()),
        }
    }

    #[allow(
        clippy::useless_conversion,
        clippy::unnecessary_cast,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn from_stat(s: &rustix::fs::Stat) -> FileStat {
        let mode = s.st_mode as u32;
        FileStat {
            kind: kind_of(mode),
            dev: s.st_dev as u64,
            ino: s.st_ino as u64,
            mode,
            nlink: s.st_nlink as u64,
            size: s.st_size as u64,
            mtime: (s.st_mtime as i64, s.st_mtime_nsec as i64),
            ctime: (s.st_ctime as i64, s.st_ctime_nsec as i64),
            owned: s.st_uid == uid(),
            owner_only: !grants_group_or_other(mode),
        }
    }

    pub(super) fn stat_file(file: &File) -> io::Result<FileStat> {
        file.metadata().map(|m| from_metadata(&m))
    }

    pub(super) fn stat_nofollow(path: &Path) -> io::Result<FileStat> {
        std::fs::symlink_metadata(path).map(|m| from_metadata(&m))
    }

    fn nofollow_bits() -> i32 {
        #[allow(clippy::cast_possible_wrap)]
        let bits = OFlags::NOFOLLOW.bits() as i32;
        bits
    }

    pub(super) fn open_nofollow(path: &Path) -> io::Result<File> {
        OpenOptions::new().read(true).custom_flags(nofollow_bits()).open(path)
    }

    pub(super) fn create_new_nofollow(path: &Path, unix_mode: u32) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(unix_mode)
            .custom_flags(nofollow_bits())
            .open(path)
    }

    pub(super) fn open_or_create_nofollow(path: &Path, unix_mode: u32) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(unix_mode)
            .custom_flags(nofollow_bits())
            .open(path)
    }

    pub(super) fn create_dir_owner_only(path: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }

    pub(super) fn create_dir_all_owner_only(path: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
    }

    pub(super) fn set_owner_only(path: &Path, directory: bool) -> io::Result<()> {
        let mode = if directory { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }

    pub(super) fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        file.read_at(buffer, offset)
    }

    pub(super) fn fsync_dir(path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }

    pub(super) fn monotonic_ns() -> i64 {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        #[allow(clippy::useless_conversion)]
        let (sec, nsec) = (i64::from(t.tv_sec), i64::from(t.tv_nsec));
        sec.saturating_mul(1_000_000_000).saturating_add(nsec)
    }

    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::DIRECTORY
    }

    #[derive(Debug)]
    pub(super) struct Dir {
        fd: OwnedFd,
    }

    impl Dir {
        pub(super) fn open(path: &Path) -> io::Result<Dir> {
            let fd = rustix::fs::open(path, directory_flags(), Mode::empty())?;
            Ok(Dir { fd })
        }

        pub(super) fn stat(&self) -> io::Result<FileStat> {
            Ok(from_stat(&rustix::fs::fstat(&self.fd)?))
        }

        pub(super) fn stat_at(&self, name: &str) -> io::Result<FileStat> {
            Ok(from_stat(&rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW)?))
        }

        pub(super) fn open_dir_at(&self, name: &str) -> io::Result<Dir> {
            let fd = rustix::fs::openat(&self.fd, name, directory_flags(), Mode::empty())?;
            Ok(Dir { fd })
        }

        pub(super) fn open_file_at(&self, name: &str) -> io::Result<File> {
            let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            Ok(File::from(rustix::fs::openat(&self.fd, name, flags, Mode::empty())?))
        }

        pub(super) fn sync_all(&self) -> io::Result<()> {
            Ok(rustix::fs::fsync(&self.fd)?)
        }

        pub(super) fn entries(&self) -> io::Result<BTreeSet<String>> {
            let mut dir = rustix::fs::Dir::read_from(&self.fd)?;
            let mut out = BTreeSet::new();
            while let Some(entry) = dir.read() {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name != "." && name != ".." {
                    out.insert(name);
                }
            }
            Ok(out)
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::{FileExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use std::time::Instant;

    use windows_permissions::constants::{AceFlags, AceType, SeObjectType, SecurityInformation};
    use windows_permissions::wrappers::{
        ConvertSidToStringSid, ConvertStringSecurityDescriptorToSecurityDescriptor, GetSecurityInfo,
        SetSecurityInfo,
    };

    use super::{FileKind, FileStat};

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_ATTRIBUTE_READONLY: u64 = 0x0000_0001;
    const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x0000_0010;
    const OWNER_RIGHTS_SID: &str = "S-1-3-4";
    const UNIX_EPOCH_AS_FILETIME: i128 = 116_444_736_000_000_000;

    fn loop_error() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidInput, "refusing to follow a symbolic link or junction")
    }

    fn filetime(value: Option<u64>) -> (i64, i64) {
        let hundreds = i128::from(value.unwrap_or(0)) - UNIX_EPOCH_AS_FILETIME;
        let seconds = hundreds.div_euclid(10_000_000);
        let nanos = hundreds.rem_euclid(10_000_000) * 100;
        (i64::try_from(seconds).unwrap_or(i64::MIN), i64::try_from(nanos).unwrap_or(0))
    }

    fn process_owner() -> Option<&'static OsString> {
        static OWNER: OnceLock<Option<OsString>> = OnceLock::new();
        OWNER
            .get_or_init(|| {
                let probe = std::env::temp_dir().join(format!(
                    ".implexity-owner-probe-{}-{}",
                    std::process::id(),
                    super::monotonic_ns()
                ));
                let owner = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&probe)
                    .ok()
                    .and_then(|file| owner_sid(&file).ok());
                let _ = std::fs::remove_file(&probe);
                owner
            })
            .as_ref()
    }

    fn owner_sid(file: &File) -> io::Result<OsString> {
        let sd = GetSecurityInfo(file, SeObjectType::SE_FILE_OBJECT, SecurityInformation::Owner)?;
        let owner = sd.owner().ok_or_else(|| io::Error::other("object has no owner"))?;
        ConvertSidToStringSid(owner)
    }

    fn ownership(file: &File) -> (bool, bool) {
        let info = SecurityInformation::Owner | SecurityInformation::Dacl;
        let Ok(sd) = GetSecurityInfo(file, SeObjectType::SE_FILE_OBJECT, info) else {
            return (false, false);
        };
        let Some(owner) = sd.owner().and_then(|sid| ConvertSidToStringSid(sid).ok()) else {
            return (false, false);
        };
        let owned = process_owner() == Some(&owner);

        let owner_only = sd.dacl().is_some_and(|acl| {

            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (0..acl.len()).all(|index| {
                    let Some(ace) = acl.get_ace(index) else { return false };
                    let allows = matches!(
                        ace.ace_type(),
                        AceType::ACCESS_ALLOWED_ACE_TYPE
                            | AceType::ACCESS_ALLOWED_CALLBACK_ACE_TYPE
                            | AceType::ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE
                            | AceType::ACCESS_ALLOWED_OBJECT_ACE_TYPE
                    );
                    if !allows || ace.flags().contains(AceFlags::InheritOnly) {
                        return true;
                    }
                    ace.sid()
                        .and_then(|sid| ConvertSidToStringSid(sid).ok())
                        .is_some_and(|sid| sid == owner || sid == OWNER_RIGHTS_SID)
                })
            }))
            .unwrap_or(false)
        });
        (owned, owner_only)
    }

    fn stat_handle(file: &File) -> io::Result<FileStat> {
        let meta = file.metadata()?;
        let info = winapi_util::file::information(file)?;
        let file_type = meta.file_type();
        let attributes = info.file_attributes();
        let kind = if file_type.is_symlink() {
            FileKind::Symlink
        } else if file_type.is_dir() {
            FileKind::Directory
        } else if file_type.is_file() {
            FileKind::Regular
        } else {
            FileKind::Other
        };

        let mut mode: u32 = if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 { 0o040_111 } else { 0o100_000 };
        mode |= if attributes & FILE_ATTRIBUTE_READONLY != 0 { 0o444 } else { 0o666 };
        if kind == FileKind::Symlink {
            mode = (mode & 0o7777) | 0o120_000;
        }
        let (owned, owner_only) = ownership(file);
        Ok(FileStat {
            kind,
            dev: info.volume_serial_number(),
            ino: info.file_index(),
            mode,
            nlink: info.number_of_links(),
            size: info.file_size(),
            mtime: filetime(info.last_write_time()),
            ctime: filetime(info.creation_time()),
            owned,
            owner_only,
        })
    }

    fn open_metadata(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    pub(super) fn stat_file(file: &File) -> io::Result<FileStat> {
        stat_handle(file)
    }

    pub(super) fn stat_nofollow(path: &Path) -> io::Result<FileStat> {
        stat_handle(&open_metadata(path)?)
    }

    pub(super) fn open_nofollow(path: &Path) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if file.metadata()?.file_type().is_symlink() {
            return Err(loop_error());
        }
        Ok(file)
    }

    fn restrict_to_owner(file: &mut File, directory: bool) -> io::Result<()> {
        let owner = owner_sid(file)?;
        let owner = owner.to_str().ok_or_else(|| io::Error::other("owner SID is not text"))?;
        let inherit = if directory { "OICI" } else { "" };
        let template =
            ConvertStringSecurityDescriptorToSecurityDescriptor(&format!("D:P(A;{inherit};FA;;;{owner})"))?;
        let dacl = template.dacl().ok_or_else(|| io::Error::other("owner-only DACL was not built"))?;
        SetSecurityInfo(
            file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            Some(dacl),
            None,
        )
    }

    fn create_new_restricted(path: &Path, unix_mode: u32, read: bool) -> io::Result<File> {
        let read_access = if read { GENERIC_READ } else { 0 };
        let mut file = OpenOptions::new()
            .read(read)
            .write(true)
            .create_new(true)
            .access_mode(read_access | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let group_or_other = unix_mode & 0o077;
        if group_or_other == 0
            && let Err(e) = restrict_to_owner(&mut file, false)
        {
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(e);
        }
        Ok(file)
    }

    pub(super) fn create_new_nofollow(path: &Path, unix_mode: u32) -> io::Result<File> {
        create_new_restricted(path, unix_mode, false)
    }

    pub(super) fn open_or_create_nofollow(path: &Path, unix_mode: u32) -> io::Result<File> {

        for _ in 0..2 {
            match create_new_restricted(path, unix_mode, true) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                created => return created,
            }
            let existing = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path);
            match existing {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
                Ok(file) => {
                    if file.metadata()?.file_type().is_symlink() {
                        return Err(loop_error());
                    }
                    return Ok(file);
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "file vanished while being opened"))
    }

    fn open_for_dacl(path: &Path) -> io::Result<File> {
        let file = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if file.metadata()?.file_type().is_symlink() {
            return Err(loop_error());
        }
        Ok(file)
    }

    pub(super) fn create_dir_owner_only(path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)?;
        let restricted = open_for_dacl(path).and_then(|mut dir| restrict_to_owner(&mut dir, true));
        if let Err(e) = restricted {
            let _ = std::fs::remove_dir(path);
            return Err(e);
        }
        Ok(())
    }

    pub(super) fn create_dir_all_owner_only(path: &Path) -> io::Result<()> {
        if path.is_dir() {
            return Ok(());
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            create_dir_all_owner_only(parent)?;
        }
        match create_dir_owner_only(path) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
            other => other,
        }
    }

    pub(super) fn set_owner_only(path: &Path, directory: bool) -> io::Result<()> {
        let mut file = open_for_dacl(path)?;
        restrict_to_owner(&mut file, directory)
    }

    pub(super) fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        file.seek_read(buffer, offset)
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn fsync_dir(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn monotonic_ns() -> i64 {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        let elapsed = ORIGIN.get_or_init(Instant::now).elapsed().as_nanos();
        i64::try_from(elapsed).unwrap_or(i64::MAX - 1).saturating_add(1)
    }

    #[derive(Debug)]
    pub(super) struct Dir {
        handle: File,
        path: PathBuf,
        identity: (u64, u64),
    }

    impl Dir {
        pub(super) fn open(path: &Path) -> io::Result<Dir> {
            let path = std::path::absolute(path)?;

            let handle = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)?;
            let file_type = handle.metadata()?.file_type();
            if file_type.is_symlink() {
                return Err(loop_error());
            }
            if !file_type.is_dir() {
                return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a directory"));
            }
            let info = winapi_util::file::information(&handle)?;
            Ok(Dir { handle, path, identity: (info.volume_serial_number(), info.file_index()) })
        }

        fn verify_bound(&self) -> io::Result<()> {
            let info = winapi_util::file::information(&open_metadata(&self.path)?)?;
            if (info.volume_serial_number(), info.file_index()) != self.identity {
                return Err(io::Error::other("directory was replaced while in use"));
            }
            Ok(())
        }

        fn checked<T>(&self, result: io::Result<T>) -> io::Result<T> {
            let value = result?;
            self.verify_bound()?;
            Ok(value)
        }

        pub(super) fn stat(&self) -> io::Result<FileStat> {
            stat_handle(&self.handle)
        }

        pub(super) fn stat_at(&self, name: &str) -> io::Result<FileStat> {
            self.checked(stat_nofollow(&self.path.join(name)))
        }

        pub(super) fn open_dir_at(&self, name: &str) -> io::Result<Dir> {
            self.checked(Dir::open(&self.path.join(name)))
        }

        pub(super) fn open_file_at(&self, name: &str) -> io::Result<File> {
            self.checked(open_nofollow(&self.path.join(name)))
        }

        #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
        pub(super) fn sync_all(&self) -> io::Result<()> {
            Ok(())
        }

        pub(super) fn entries(&self) -> io::Result<BTreeSet<String>> {
            let mut out = BTreeSet::new();
            for entry in std::fs::read_dir(&self.path)? {
                out.insert(entry?.file_name().to_string_lossy().into_owned());
            }
            self.checked(Ok(out))
        }
    }
}

