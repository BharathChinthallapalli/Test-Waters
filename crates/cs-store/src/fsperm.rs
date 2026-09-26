//! Owner-only files and directories (R1.4, R2.2, feature 02 design "Paths and
//! permissions").
//!
//! Permissions are set when a file or directory is created, never changed
//! afterwards, so there is no moment when another user can open it.
//! - Unix: directories `0700`, files `0600`.
//! - Windows: a protected DACL granting only the current user; implemented by
//!   feature 02 unit `win-acl`. Until then these functions return
//!   [`io::ErrorKind::Unsupported`] on Windows.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Creates `path` and any missing parents. Directories this call creates are
/// owner-only; existing ones are left as they are.
pub fn create_dir_all_owner_only(path: &Path) -> io::Result<()> {
    imp::create_dir_all(path)
}

/// Creates a new owner-only file. Fails if `path` already exists.
pub fn create_new_owner_only(path: &Path) -> io::Result<File> {
    imp::create_new(path)
}

/// Replaces `path` with `bytes` atomically: a reader sees the old contents or the
/// new ones, never a mix. The new file is owner-only from its creation.
pub fn write_owner_only_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(io::Error::other)?;
    let mut temp_name = name.to_os_string();
    temp_name.push(format!(".{}.tmp", hex::encode(suffix)));
    let temp = dir.join(temp_name);

    let written = create_new_owner_only(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let renamed = written.and_then(|()| std::fs::rename(&temp, path));
    if renamed.is_err() {
        // Best effort: the temporary file holds nothing the target wouldn't.
        let _ = std::fs::remove_file(&temp);
    }
    renamed
}

#[cfg(unix)]
mod imp {
    use std::fs::{DirBuilder, File, OpenOptions};
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::path::Path;

    pub(super) fn create_dir_all(path: &Path) -> io::Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(path)
    }

    pub(super) fn create_new(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
}

#[cfg(not(unix))]
mod imp {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "owner-only files are not implemented on this platform yet",
        )
    }

    pub(super) fn create_dir_all(_path: &Path) -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn create_new(_path: &Path) -> io::Result<File> {
        Err(unsupported())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn created_directories_are_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a").join("b");

        create_dir_all_owner_only(&nested).unwrap();

        assert_eq!(mode(&root.path().join("a")), 0o700);
        assert_eq!(mode(&nested), 0o700);
    }

    #[test]
    fn created_files_are_owner_only_and_never_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("f");

        create_new_owner_only(&path).unwrap();

        assert_eq!(mode(&path), 0o600);
        assert_eq!(
            create_new_owner_only(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn atomic_write_replaces_contents_and_leaves_no_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("control-token");

        write_owner_only_atomic(&path, b"first").unwrap();
        write_owner_only_atomic(&path, b"second").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
