//! Owner-only files and directories (R1.4, R2.2, feature 02 design "Paths and
//! permissions").
//!
//! Permissions are set when a file or directory is created, never changed
//! afterwards, so there is no moment when another user can open it.
//! - Unix: directories `0700`, files `0600`.
//! - Windows: a protected DACL (`D:P`, so nothing is inherited from the parent)
//!   whose only ACE grants the current user's SID full access. The descriptor
//!   is passed to `CreateDirectoryW` / `CreateFileW` in `SECURITY_ATTRIBUTES`.
//!   The directory ACE is inheritable (`OICI`): files and directories that other
//!   code creates inside a directory made here, without a descriptor of their
//!   own (SQLite's `-wal` and `-shm`), get it as their only, inherited ACE. Their
//!   DACL is not protected, though, so a later change to the parent's DACL
//!   reaches them; and a directory that already existed keeps its own ACEs.
//!   [`owner_only_problem`] tells whether an existing directory is owner-only.
//! - Anything else: these functions return [`io::ErrorKind::Unsupported`].

use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Creates `path` and any missing parents. Directories this call creates are
/// owner-only; existing ones are left as they are.
pub fn create_dir_all_owner_only(path: &Path) -> io::Result<()> {
    imp::create_dir_all(path)
}

/// Why an existing directory is not owner-only on Windows (see
/// [`owner_only_problem`]). The `Display` text completes "the directory …".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclProblem {
    /// There is no DACL, which allows everyone full access, or an ACE allows
    /// access to an account other than the current user. An allow ACE of a kind
    /// the check doesn't read (object or callback ACEs) counts as one.
    OthersAllowed,
    /// The DACL is not protected, so inheritable ACEs of the parent reach it.
    Inherited,
    /// The owner is not the current user. An owner can always read and change
    /// the DACL (`READ_CONTROL`, `WRITE_DAC`).
    NotOwner,
}

impl fmt::Display for AclProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OthersAllowed => "grants access to other accounts",
            Self::Inherited => "inherits permissions from its parent",
            Self::NotOwner => "is not owned by the current user",
        })
    }
}

/// Windows: checks that the directory `path` has the descriptor this module
/// gives the directories it creates: owned by the current user, with a
/// protected DACL whose allow ACEs are all for the current user (deny ACEs are
/// fine). `Ok(None)` when it does, else the first problem found, in the order
/// of [`AclProblem`]. The directory is never changed.
#[cfg(windows)]
pub fn owner_only_problem(path: &Path) -> io::Result<Option<AclProblem>> {
    imp::owner_only_problem(path)
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

/// Windows: every object is created with its final security descriptor, built
/// from SDDL (Microsoft Learn, "Creating a DACL", "Security Descriptor String
/// Format", "ACE Strings"):
/// - `D:P` sets `SE_DACL_PROTECTED`, so no inheritable ACE from a parent is
///   merged in;
/// - the single ACE `(A;<flags>;FA;;;<SID>)` allows `FILE_ALL_ACCESS` to the SID
///   of the user that owns the process token (`GetTokenInformation(TokenUser)`).
///
/// There is never a NULL DACL: the SDDL format cannot express one, and a
/// descriptor without our ACE is never passed to a create call.
///
/// Paths are made absolute and always given the `\\?\` prefix, like std's
/// `get_long_path` (`library/std/src/sys/path/windows.rs`) except that std leaves
/// short absolute paths unprefixed; either way paths longer than `MAX_PATH` work.
#[cfg(windows)]
#[allow(unsafe_code)] // FFI to Win32; each unsafe block states why it is sound.
mod imp {
    use std::ffi::{OsStr, c_void};
    use std::fs::File;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::ptr;
    use std::sync::OnceLock;

    use super::AclProblem;
    use windows_sys::Win32::Foundation::{
        ERROR_INSUFFICIENT_BUFFER, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        ConvertStringSidToSidW, GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
        GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// A buffer that a Win32 function allocated for us and documents as freed
    /// with `LocalFree`. Freed exactly once, on drop.
    struct LocalBuf(*mut c_void);

    impl Drop for LocalBuf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` came from a Win32 call that allocates with
                // LocalAlloc; this is the only owner, and it is freed only here.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Kind {
        Directory,
        File,
    }

    pub(super) fn create_dir_all(path: &Path) -> io::Result<()> {
        let descriptor = owner_only_descriptor(Kind::Directory)?;
        create_dir_all_with(path, &descriptor)
    }

    /// Mirrors `std::fs::create_dir_all`: try the leaf, create missing parents
    /// on `NotFound`, and accept a directory that already exists (possibly
    /// created concurrently). Existing directories keep their permissions.
    fn create_dir_all_with(path: &Path, descriptor: &LocalBuf) -> io::Result<()> {
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        match create_dir(path, descriptor) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) if path.is_dir() => return Ok(()),
            Err(error) => return Err(error),
        }
        match path.parent() {
            Some(parent) => create_dir_all_with(parent, descriptor)?,
            None => {
                return Err(io::Error::other(
                    "failed to create the whole directory tree",
                ));
            }
        }
        match create_dir(path, descriptor) {
            Ok(()) => Ok(()),
            Err(_) if path.is_dir() => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn create_dir(path: &Path, descriptor: &LocalBuf) -> io::Result<()> {
        let path = wide_path(path)?;
        let attributes = security_attributes(descriptor);
        // SAFETY: `path` is NUL-terminated and `attributes` points at a valid
        // self-relative descriptor; both outlive the call, which only reads them.
        if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn create_new(path: &Path) -> io::Result<File> {
        let descriptor = owner_only_descriptor(Kind::File)?;
        let path = wide_path(path)?;
        let attributes = security_attributes(&descriptor);
        // SAFETY: `path` is NUL-terminated and `attributes` points at a valid
        // self-relative descriptor; both outlive the call, which only reads them.
        // A null template handle is allowed.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_WRITE,
                // The same share mode `std::fs::OpenOptions` uses.
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateFileW succeeded, so `handle` is an open file handle that
        // nothing else owns; `OwnedHandle` closes it exactly once.
        Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    fn security_attributes(descriptor: &LocalBuf) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        }
    }

    fn owner_only_descriptor(kind: Kind) -> io::Result<LocalBuf> {
        let sid = current_user_sid()?;
        let ace_flags = match kind {
            // Object- and container-inherit: children created without their own
            // descriptor inherit this ACE. (The design writes OICI for every
            // object; on a file the flags would have no effect.)
            Kind::Directory => "OICI",
            Kind::File => "",
        };
        // `O:` makes the user the owner too. Without it an elevated process's
        // objects are owned by BUILTIN\Administrators, and owners implicitly get
        // READ_CONTROL and WRITE_DAC.
        descriptor_from_sddl(&format!("O:{sid}D:P(A;{ace_flags};FA;;;{sid})"))
    }

    fn descriptor_from_sddl(sddl: &str) -> io::Result<LocalBuf> {
        let sddl = to_wide(OsStr::new(sddl))?;
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated and outlives the call; `descriptor`
        // is a valid out pointer; the size out pointer may be null.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        let descriptor = LocalBuf(descriptor);
        if descriptor.0.is_null() {
            return Err(io::Error::other("no security descriptor was returned"));
        }
        Ok(descriptor)
    }

    // ACE types (Microsoft Learn, `ACE_HEADER`). The values are winnt.h's, as
    // windows-sys 0.61.2 gives them in `Win32/System/SystemServices`, a feature
    // this crate doesn't enable.
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    const ACCESS_DENIED_OBJECT_ACE_TYPE: u8 = 6;
    const ACCESS_DENIED_CALLBACK_ACE_TYPE: u8 = 10;
    const ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE: u8 = 12;

    pub(super) fn owner_only_problem(path: &Path) -> io::Result<Option<AclProblem>> {
        let path = wide_path(path)?;
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `path` is NUL-terminated; the SID and ACL out pointers may
        // be null (they are read from the descriptor below); `descriptor` is a
        // valid out pointer.
        let status = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let descriptor = LocalBuf(descriptor);
        if descriptor.0.is_null() {
            return Err(io::Error::other("no security descriptor was returned"));
        }
        descriptor_problem(&descriptor)
    }

    /// The checks of [`owner_only_problem`] on `descriptor`, which must hold a
    /// valid self-relative security descriptor (from `GetNamedSecurityInfoW` or
    /// SDDL); it is only read.
    fn descriptor_problem(descriptor: &LocalBuf) -> io::Result<Option<AclProblem>> {
        let user = user_sid()?;

        let mut present = 0;
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut defaulted = 0;
        // SAFETY: `descriptor` is valid (see above); the out pointers are valid.
        let read = unsafe {
            GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        // No DACL, or a NULL one, allows everyone full access
        // (`GetSecurityDescriptorDacl`, pDacl: "fail securely").
        if present == 0 || dacl.is_null() {
            return Ok(Some(AclProblem::OthersAllowed));
        }
        // SAFETY: `dacl` points into `descriptor`, which outlives the call;
        // `user` holds a valid SID.
        let only_user = unsafe { only_user_allowed(dacl, user.0) }?;
        if !only_user {
            return Ok(Some(AclProblem::OthersAllowed));
        }

        let mut control = 0;
        let mut revision = 0;
        // SAFETY: `descriptor` is valid; the out pointers are valid.
        if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if control & SE_DACL_PROTECTED == 0 {
            return Ok(Some(AclProblem::Inherited));
        }

        let mut owner: PSID = ptr::null_mut();
        let mut owner_defaulted = 0;
        // SAFETY: `descriptor` is valid; the out pointers are valid.
        if unsafe { GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut owner_defaulted) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a non-null `owner` points at a SID inside `descriptor`, and
        // `user` holds a valid SID; EqualSid is only given SIDs that IsValidSid
        // accepts, as its documentation requires.
        let is_user =
            !owner.is_null() && unsafe { IsValidSid(owner) != 0 && EqualSid(owner, user.0) != 0 };
        Ok((!is_user).then_some(AclProblem::NotOwner))
    }

    /// Whether every ACE of `dacl` that can allow access is an
    /// `ACCESS_ALLOWED_ACE` for `user`. Deny ACEs never allow access.
    ///
    /// # Safety
    /// `dacl` must point at a valid ACL and `user` at a valid SID, both
    /// readable during the call.
    unsafe fn only_user_allowed(dacl: *const ACL, user: PSID) -> io::Result<bool> {
        // SAFETY: the caller guarantees `dacl` points at a valid ACL.
        let count = unsafe { (*dacl).AceCount };
        for index in 0..u32::from(count) {
            let mut ace: *mut c_void = ptr::null_mut();
            // SAFETY: `dacl` is valid and `index` is below its ACE count; `ace`
            // is a valid out pointer.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: GetAce returned the address of an ACE inside the ACL.
            // Every ACE starts with an ACE_HEADER (`ACE_HEADER`, Remarks) and
            // is DWORD-aligned (`ACCESS_ALLOWED_ACE`, Remarks).
            let header = unsafe { ace.cast::<ACE_HEADER>().read() };
            match header.AceType {
                ACCESS_DENIED_ACE_TYPE
                | ACCESS_DENIED_OBJECT_ACE_TYPE
                | ACCESS_DENIED_CALLBACK_ACE_TYPE
                | ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE => {}
                ACCESS_ALLOWED_ACE_TYPE
                    if usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>() =>
                {
                    // SAFETY: an ACE of this type is an ACCESS_ALLOWED_ACE, at
                    // least that large (checked above), whose SID starts at
                    // `SidStart` inside the ACE.
                    let sid: PSID =
                        unsafe { (&raw mut (*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast() };
                    // SAFETY: `sid` points into the ACL and `user` at a valid
                    // SID; EqualSid is only given SIDs that IsValidSid accepts.
                    if unsafe { IsValidSid(sid) == 0 || EqualSid(sid, user) == 0 } {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    /// The current user's SID in binary form.
    fn user_sid() -> io::Result<LocalBuf> {
        let string = to_wide(OsStr::new(current_user_sid()?))?;
        let mut sid: PSID = ptr::null_mut();
        // SAFETY: `string` is NUL-terminated and outlives the call; `sid` is a
        // valid out pointer.
        if unsafe { ConvertStringSidToSidW(string.as_ptr(), &mut sid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let sid = LocalBuf(sid);
        if sid.0.is_null() {
            return Err(io::Error::other("no SID was returned"));
        }
        Ok(sid)
    }

    /// The string SID (`S-1-5-21-…`) of the user in this process's token. The
    /// process token's user never changes, so it is looked up once.
    pub(super) fn current_user_sid() -> io::Result<&'static str> {
        static SID: OnceLock<String> = OnceLock::new();
        if let Some(sid) = SID.get() {
            return Ok(sid);
        }
        let sid = query_user_sid()?;
        Ok(SID.get_or_init(|| sid))
    }

    fn query_user_sid() -> io::Result<String> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // closing; `token` is a valid out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcessToken succeeded, so `token` is an open handle that
        // nothing else owns; `OwnedHandle` closes it exactly once.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };

        let mut needed = 0u32;
        // SAFETY: a null buffer with length 0 only asks for the size, written to
        // `needed`, a valid out pointer.
        let sized = unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if sized == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
                return Err(error);
            }
        }
        // TOKEN_USER starts with a pointer, so use a pointer-aligned buffer.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>()).max(1)];
        let length = u32::try_from(buffer.len() * size_of::<u64>())
            .map_err(|_| io::Error::other("token information is too large"))?;
        // SAFETY: `buffer` is writable for `length` bytes and 8-byte aligned;
        // `needed` is a valid out pointer.
        let filled = unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut needed,
            )
        };
        if filled == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: on success the buffer starts with a TOKEN_USER, and its SID
        // pointer points into `buffer`, which stays alive and unmodified below.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };

        let mut string: *mut u16 = ptr::null_mut();
        // SAFETY: `sid` is a valid SID (see above); `string` is a valid out pointer.
        if unsafe { ConvertSidToStringSidW(sid, &mut string) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let string = LocalBuf(string.cast());
        // SAFETY: ConvertSidToStringSidW returned a NUL-terminated wide string,
        // kept alive by `string` until after the copy.
        unsafe { string_from_wide(string.0.cast()) }
    }

    /// Copies a NUL-terminated UTF-16 string.
    ///
    /// # Safety
    /// `wide` must be non-null and point to a NUL-terminated UTF-16 string that
    /// stays valid for reads during the call.
    unsafe fn string_from_wide(wide: *const u16) -> io::Result<String> {
        if wide.is_null() {
            return Err(io::Error::other("Windows returned no string"));
        }
        let mut len = 0;
        // SAFETY: the caller guarantees a NUL terminator, so every unit up to and
        // including it is readable.
        while unsafe { *wide.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: the `len` units before the terminator were just read.
        let units = unsafe { std::slice::from_raw_parts(wide, len) };
        String::from_utf16(units)
            .map_err(|_| io::Error::other("Windows returned a string that is not UTF-16"))
    }

    /// A NUL-terminated wide string; an interior NUL would silently truncate it.
    fn to_wide(value: &OsStr) -> io::Result<Vec<u16>> {
        let mut wide: Vec<u16> = value.encode_wide().collect();
        if wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "string contains a NUL character",
            ));
        }
        wide.push(0);
        Ok(wide)
    }

    /// `path` in verbatim form, like std's `get_long_path` with
    /// `prefer_verbatim` (`library/std/src/sys/path/windows.rs`, Rust 1.98.1) but
    /// for every length: made absolute with `GetFullPathNameW` (through `std::path::absolute`),
    /// then given the `\\?\` or `\\?\UNC\` prefix, which lifts the `MAX_PATH`
    /// limit (Microsoft Learn, "Naming Files, Paths, and Namespaces"). Paths that
    /// are already verbatim are passed unchanged.
    fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
        const SEP: u16 = b'\\' as u16;
        const VERBATIM: &[u16] = &[SEP, SEP, b'?' as u16, SEP];
        const NT: &[u16] = &[SEP, b'?' as u16, b'?' as u16, SEP];
        const DEVICE: &[u16] = &[SEP, SEP, b'.' as u16, SEP];
        const UNC: &[u16] = &[
            SEP,
            SEP,
            b'?' as u16,
            SEP,
            b'U' as u16,
            b'N' as u16,
            b'C' as u16,
            SEP,
        ];

        let given = to_wide(path.as_os_str())?;
        if given.starts_with(VERBATIM) || given.starts_with(NT) {
            return Ok(given);
        }
        let absolute = to_wide(std::path::absolute(path)?.as_os_str())?;
        let (prefix, rest): (&[u16], &[u16]) = match absolute.as_slice() {
            // C:\ => \\?\C:\
            [_, colon, SEP, ..] if *colon == u16::from(b':') => (VERBATIM, &absolute),
            // \\.\ => \\?\
            [SEP, SEP, dot, SEP, ..] if *dot == u16::from(b'.') => {
                (VERBATIM, &absolute[DEVICE.len()..])
            }
            // \\?\ and \??\ stay as they are.
            rest if rest.starts_with(VERBATIM) || rest.starts_with(NT) => (&[], rest),
            // \\server\share => \\?\UNC\server\share
            [SEP, SEP, ..] => (UNC, &absolute[2..]),
            rest => (&[], rest),
        };
        let mut wide = Vec::with_capacity(prefix.len() + rest.len());
        wide.extend_from_slice(prefix);
        wide.extend_from_slice(rest);
        Ok(wide)
    }

    /// Test helpers: read a DACL back as SDDL, and normalise an SDDL string the
    /// same way (so SID aliases and flag order compare equal).
    #[cfg(test)]
    pub(super) mod inspect {
        use super::*;
        use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
        use windows_sys::Win32::Security::OBJECT_SECURITY_INFORMATION;

        /// Creates the directory `path` with the descriptor `sddl`.
        pub(in super::super) fn create_dir_with_sddl(path: &Path, sddl: &str) -> io::Result<()> {
            create_dir(path, &descriptor_from_sddl(sddl)?)
        }

        /// [`owner_only_problem`] for the in-memory descriptor `sddl`.
        pub(in super::super) fn sddl_problem(sddl: &str) -> io::Result<Option<AclProblem>> {
            descriptor_problem(&descriptor_from_sddl(sddl)?)
        }

        pub(in super::super) fn dacl_sddl(path: &Path) -> io::Result<String> {
            security_sddl(path, DACL_SECURITY_INFORMATION)
        }

        pub(in super::super) fn owner_sddl(path: &Path) -> io::Result<String> {
            security_sddl(path, OWNER_SECURITY_INFORMATION)
        }

        fn security_sddl(path: &Path, info: OBJECT_SECURITY_INFORMATION) -> io::Result<String> {
            let path = wide_path(path)?;
            let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
            // SAFETY: `path` is NUL-terminated; the SID and ACL out pointers may
            // be null; `descriptor` is a valid out pointer.
            let status = unsafe {
                GetNamedSecurityInfoW(
                    path.as_ptr(),
                    SE_FILE_OBJECT,
                    info,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut descriptor,
                )
            };
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            to_sddl(&LocalBuf(descriptor), info)
        }

        pub(in super::super) fn normalise(sddl: &str) -> io::Result<String> {
            to_sddl(&descriptor_from_sddl(sddl)?, DACL_SECURITY_INFORMATION)
        }

        pub(in super::super) fn normalise_owner(sddl: &str) -> io::Result<String> {
            to_sddl(&descriptor_from_sddl(sddl)?, OWNER_SECURITY_INFORMATION)
        }

        fn to_sddl(descriptor: &LocalBuf, info: OBJECT_SECURITY_INFORMATION) -> io::Result<String> {
            let mut string: *mut u16 = ptr::null_mut();
            // SAFETY: `descriptor` holds a valid descriptor; `string` is a valid
            // out pointer; the length out pointer may be null.
            let converted = unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor.0,
                    SDDL_REVISION_1,
                    info,
                    &mut string,
                    ptr::null_mut(),
                )
            };
            if converted == 0 {
                return Err(io::Error::last_os_error());
            }
            let string = LocalBuf(string.cast());
            // SAFETY: the call returned a NUL-terminated wide string, kept alive
            // by `string` until after the copy.
            unsafe { string_from_wide(string.0.cast()) }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "owner-only files are not implemented on this platform",
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

#[cfg(all(test, windows))]
mod tests {
    use super::imp::{current_user_sid, inspect};
    use super::*;

    /// Splits a DACL-only SDDL string, `D:<flags>(ace)(ace)…`, into its flags
    /// and its ACE strings.
    fn parse_dacl(sddl: &str) -> (String, Vec<String>) {
        let rest = sddl
            .strip_prefix("D:")
            .unwrap_or_else(|| panic!("no DACL in {sddl:?}"));
        let (flags, aces) = rest.split_at(rest.find('(').unwrap_or(rest.len()));
        let aces = aces
            .split(')')
            .filter(|ace| !ace.is_empty())
            .map(|ace| ace.trim_start_matches('(').to_owned())
            .collect();
        (flags.to_owned(), aces)
    }

    /// `path` is owned by the current user, and its DACL is protected and holds
    /// exactly one ACE: allow full access to the current user, with `ace_flags`.
    fn assert_owner_only(path: &Path, ace_flags: &str) {
        assert_single_ace(path, ace_flags, true);
        let sid = current_user_sid().unwrap();
        let expected = inspect::normalise_owner(&format!("O:{sid}")).unwrap();
        let actual = inspect::owner_sddl(path).unwrap();
        assert_eq!(actual, expected, "owner of {}", path.display());
    }

    /// The DACL of `path` holds exactly one ACE, allowing full access to the
    /// current user with `ace_flags`, and is protected or not.
    fn assert_single_ace(path: &Path, ace_flags: &str, protected: bool) {
        let sid = current_user_sid().unwrap();
        // Normalised through Windows, so the expected ACE is spelled the way
        // Windows prints it (for example a well-known SID alias).
        let expected = inspect::normalise(&format!("D:(A;{ace_flags};FA;;;{sid})")).unwrap();
        let (_, expected_aces) = parse_dacl(&expected);
        assert_eq!(expected_aces.len(), 1, "{expected}");

        let actual = inspect::dacl_sddl(path).unwrap();
        let (flags, aces) = parse_dacl(&actual);
        assert_eq!(
            flags.contains('P'),
            protected,
            "DACL of {}: {actual}",
            path.display()
        );
        assert_eq!(aces, expected_aces, "DACL of {}: {actual}", path.display());
    }

    #[test]
    fn current_user_sid_is_a_string_sid() {
        let sid = current_user_sid().unwrap();
        let rest = sid
            .strip_prefix("S-1-")
            .unwrap_or_else(|| panic!("{sid:?}"));
        assert!(
            rest.chars().all(|c| c.is_ascii_digit() || c == '-'),
            "{sid:?}"
        );
    }

    #[test]
    fn created_directories_are_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let before = inspect::dacl_sddl(root.path()).unwrap();
        let nested = root.path().join("a").join("b");

        create_dir_all_owner_only(&nested).unwrap();

        assert_owner_only(&root.path().join("a"), "OICI");
        assert_owner_only(&nested, "OICI");
        // Existing directories are left as they are.
        assert_eq!(inspect::dacl_sddl(root.path()).unwrap(), before);
        create_dir_all_owner_only(&nested).unwrap();
        assert_owner_only(&nested, "OICI");
    }

    #[test]
    fn created_files_are_owner_only_and_never_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("f");

        let mut file = create_new_owner_only(&path).unwrap();
        file.write_all(b"secret").unwrap();
        drop(file);

        assert_owner_only(&path, "");
        assert_eq!(
            create_new_owner_only(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"secret");
    }

    #[test]
    fn files_in_an_owner_only_directory_are_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        create_dir_all_owner_only(&dir).unwrap();
        let path = dir.join("daemon.json");

        write_owner_only_atomic(&path, b"{}").unwrap();

        assert_owner_only(&path, "");
    }

    #[test]
    fn atomic_write_replaces_contents_and_leaves_no_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("control-token");

        write_owner_only_atomic(&path, b"first").unwrap();
        write_owner_only_atomic(&path, b"second").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_owner_only(&path, "");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn objects_created_by_other_code_inherit_only_the_owner_ace() {
        // SQLite creates its -wal and -shm files without a descriptor of its own.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        create_dir_all_owner_only(&dir).unwrap();

        let file = dir.join("callsheet.db-wal");
        std::fs::File::create(&file).unwrap();
        let subdir = dir.join("backups");
        std::fs::create_dir(&subdir).unwrap();

        assert_single_ace(&file, "ID", false);
        assert_single_ace(&subdir, "OICIID", false);
    }

    // ---- owner_only_problem ------------------------------------------------------

    #[test]
    fn created_directories_pass_the_owner_only_check() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        create_dir_all_owner_only(&dir).unwrap();

        assert_eq!(owner_only_problem(&dir).unwrap(), None);
        assert_owner_only(&dir, "OICI");
    }

    #[test]
    fn a_directory_made_by_std_fails_and_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        std::fs::create_dir(&dir).unwrap();
        let before = inspect::dacl_sddl(&dir).unwrap();

        let problem = owner_only_problem(&dir).unwrap();

        assert!(problem.is_some(), "{before}");
        let after = inspect::dacl_sddl(&dir).unwrap();
        assert_eq!(after, before);
        assert!(!parse_dacl(&after).0.contains('P'), "{after}");
    }

    #[test]
    fn an_added_everyone_ace_fails_and_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        create_dir_all_owner_only(&dir).unwrap();
        // Everyone (S-1-1-0) may read; `*` marks a numeric SID (Microsoft
        // Learn, "icacls", Remarks).
        let status = std::process::Command::new("icacls")
            .arg(&dir)
            .args(["/grant", "*S-1-1-0:(OI)(CI)(R)"])
            .status()
            .unwrap();
        assert!(status.success());
        let before = inspect::dacl_sddl(&dir).unwrap();
        assert!(before.contains(";;;WD)"), "{before}");

        assert_eq!(
            owner_only_problem(&dir).unwrap(),
            Some(AclProblem::OthersAllowed)
        );
        assert_eq!(inspect::dacl_sddl(&dir).unwrap(), before);
    }

    #[test]
    fn an_unprotected_dacl_fails_even_with_only_the_owner_ace() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("data");
        create_dir_all_owner_only(&parent).unwrap();
        let dir = parent.join("child");
        let sid = current_user_sid().unwrap();
        // Not protected: the parent's only ACE, also the user's, is merged in.
        inspect::create_dir_with_sddl(&dir, &format!("O:{sid}D:(A;OICI;FA;;;{sid})")).unwrap();

        assert_eq!(
            owner_only_problem(&dir).unwrap(),
            Some(AclProblem::Inherited)
        );
    }

    #[test]
    fn owner_only_problem_of_descriptors() {
        let sid = current_user_sid().unwrap();
        let owner_ace = format!("(A;OICI;FA;;;{sid})");
        let cases = [
            (format!("O:{sid}D:P{owner_ace}"), None),
            // Deny ACEs never grant anything.
            (format!("O:{sid}D:P(D;OICI;FW;;;WD){owner_ace}"), None),
            // An empty DACL grants nothing.
            (format!("O:{sid}D:P"), None),
            (
                format!("O:{sid}D:P{owner_ace}(A;OICI;FR;;;WD)"),
                Some(AclProblem::OthersAllowed),
            ),
            (
                format!("O:{sid}D:P{owner_ace}(A;OICI;FR;;;BU)"),
                Some(AclProblem::OthersAllowed),
            ),
            (
                format!("O:{sid}D:P{owner_ace}(A;OICIIO;FA;;;CO)"),
                Some(AclProblem::OthersAllowed),
            ),
            // An allow ACE of another kind counts, even for the user.
            (
                format!("O:{sid}D:P(OA;;FR;bf967a86-0de6-11d0-a285-00aa003049e2;;{sid})"),
                Some(AclProblem::OthersAllowed),
            ),
            // No DACL at all allows everyone full access.
            (format!("O:{sid}"), Some(AclProblem::OthersAllowed)),
            (format!("O:{sid}D:{owner_ace}"), Some(AclProblem::Inherited)),
            (format!("O:BAD:P{owner_ace}"), Some(AclProblem::NotOwner)),
            (format!("D:P{owner_ace}"), Some(AclProblem::NotOwner)),
        ];
        for (sddl, expected) in cases {
            assert_eq!(inspect::sddl_problem(&sddl).unwrap(), expected, "{sddl}");
        }
    }

    #[test]
    fn paths_longer_than_max_path_work() {
        let root = tempfile::tempdir().unwrap();
        let mut dir = root.path().to_path_buf();
        while dir.as_os_str().len() < 300 {
            dir.push("a-directory-name-of-some-length");
        }
        let path = dir.join("control-token");

        create_dir_all_owner_only(&dir).unwrap();
        write_owner_only_atomic(&path, b"token").unwrap();

        assert_owner_only(&dir, "OICI");
        assert_owner_only(&path, "");
        assert_eq!(std::fs::read(&path).unwrap(), b"token");
    }
}
