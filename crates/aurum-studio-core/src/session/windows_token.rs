//! Windows session tokens use the process token SID, never environment identity.
//! A complete protected DACL is prepared before opening a file for mutation.
use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::{null_mut, read_unaligned};
use std::time::{Duration, Instant};

type Handle = *mut c_void;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER: u32 = 1;
const MAX_SECURITY_BYTES: usize = 65536;
const FILE_ALL_ACCESS: u32 = 0x001f_01ff;
const DACL_SECURITY_INFORMATION: u32 = 4;
const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
const SE_DACL_PRESENT: u16 = 4;
const SE_DACL_PROTECTED: u16 = 0x1000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[repr(C)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}
#[repr(C)]
struct TokenUser {
    user: SidAndAttributes,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Acl {
    revision: u8,
    reserved: u8,
    bytes: u16,
    count: u16,
    reserved2: u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct AceHeader {
    kind: u8,
    flags: u8,
    bytes: u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct AllowedAce {
    header: AceHeader,
    mask: u32,
    sid_start: u32,
}
#[repr(C)]
struct SecurityDescriptor {
    revision: u8,
    reserved: u8,
    control: u16,
    owner: *mut c_void,
    group: *mut c_void,
    sacl: *mut Acl,
    dacl: *mut Acl,
}
#[repr(C)]
struct SecurityAttributes {
    bytes: u32,
    descriptor: *mut c_void,
    inherit: i32,
}
#[repr(C)]
#[derive(Default)]
struct FileInformation {
    attributes: u32,
    created: [u32; 2],
    accessed: [u32; 2],
    written: [u32; 2],
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

// Refuse an unexpected ABI at compile time instead of passing mis-sized native structures.
const _: () = {
    let wide = size_of::<usize>() == 8;
    assert!(size_of::<Acl>() == 8);
    assert!(size_of::<AllowedAce>() == 12);
    assert!(size_of::<FileInformation>() == 52);
    assert!(size_of::<TokenUser>() == if wide { 16 } else { 8 });
    assert!(size_of::<SecurityDescriptor>() == if wide { 40 } else { 20 });
    assert!(size_of::<SecurityAttributes>() == if wide { 24 } else { 12 });
};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> Handle;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        sharing: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn GetFileType(file: Handle) -> u32;
    fn GetFileInformationByHandle(file: Handle, information: *mut FileInformation) -> i32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        information: *mut c_void,
        bytes: u32,
        required: *mut u32,
    ) -> i32;
    fn IsValidSid(sid: *mut c_void) -> i32;
    fn GetLengthSid(sid: *mut c_void) -> u32;
    fn EqualSid(first: *mut c_void, second: *mut c_void) -> i32;
    fn InitializeAcl(acl: *mut Acl, bytes: u32, revision: u32) -> i32;
    fn AddAccessAllowedAceEx(
        acl: *mut Acl,
        revision: u32,
        flags: u32,
        mask: u32,
        sid: *mut c_void,
    ) -> i32;
    fn IsValidAcl(acl: *mut Acl) -> i32;
    fn GetAce(acl: *mut Acl, index: u32, ace: *mut *mut c_void) -> i32;
    fn InitializeSecurityDescriptor(descriptor: *mut c_void, revision: u32) -> i32;
    fn SetSecurityDescriptorOwner(
        descriptor: *mut c_void,
        owner: *mut c_void,
        defaulted: i32,
    ) -> i32;
    fn SetSecurityDescriptorDacl(
        descriptor: *mut c_void,
        present: i32,
        acl: *mut Acl,
        defaulted: i32,
    ) -> i32;
    fn SetSecurityDescriptorControl(descriptor: *mut c_void, interest: u16, bits: u16) -> i32;
    fn IsValidSecurityDescriptor(descriptor: *mut c_void) -> i32;
    fn GetSecurityDescriptorLength(descriptor: *mut c_void) -> u32;
    fn GetSecurityDescriptorControl(
        descriptor: *mut c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
    fn GetSecurityInfo(
        object: Handle,
        kind: u32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut Acl,
        sacl: *mut *mut Acl,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn SetSecurityInfo(
        object: Handle,
        kind: u32,
        information: u32,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut Acl,
        sacl: *mut Acl,
    ) -> u32;
}

fn invalid(stage: &'static str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("session token {stage} refused"),
    )
}
fn contextual(stage: &'static str, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "session token {stage} failed (os={:?})",
            error.raw_os_error()
        ),
    )
}
fn checked(value: i32, stage: &'static str) -> io::Result<()> {
    if value == 0 {
        Err(contextual(stage, io::Error::last_os_error()))
    } else {
        Ok(())
    }
}
fn status(value: u32, stage: &'static str) -> io::Result<()> {
    if value == 0 {
        Ok(())
    } else {
        Err(contextual(
            stage,
            io::Error::from_raw_os_error(value as i32),
        ))
    }
}
fn contains(base: *const u8, bytes: usize, pointer: *const c_void, needed: usize) -> bool {
    let start = base as usize;
    let address = pointer as usize;
    needed <= bytes && address >= start && address - start <= bytes - needed
}
fn sid_length(sid: *mut c_void, base: *const u8, bytes: usize) -> io::Result<usize> {
    if sid as usize & 3 != 0 || !contains(base, bytes, sid, 8) {
        return Err(invalid("SID range"));
    }
    // SAFETY: the fixed SID header is inside a live, bounded OS output buffer.
    let count = unsafe { *((sid as *const u8).add(1)) } as usize;
    let length = 8 + 4 * count;
    if count > 15 || !contains(base, bytes, sid, length) {
        return Err(invalid("SID length"));
    }
    // SAFETY: the complete SID, including all subauthorities, is bounded above.
    if unsafe { IsValidSid(sid) } == 0 || unsafe { GetLengthSid(sid) } as usize != length {
        return Err(invalid("SID validity"));
    }
    Ok(length)
}

struct CurrentUser {
    _buffer: Vec<usize>,
    sid: *mut c_void,
    bytes: usize,
}
impl CurrentUser {
    fn load() -> io::Result<Self> {
        let mut raw = null_mut();
        // SAFETY: the process pseudo-handle is borrowed; the output owns only TOKEN_QUERY rights.
        checked(
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) },
            "open process token",
        )?;
        if raw.is_null() {
            return Err(invalid("process token handle"));
        }
        // SAFETY: successful OpenProcessToken returned this distinct owned handle.
        let token = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut needed = 0;
        // SAFETY: a zero-length query writes only the required-size scalar.
        let result = unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TOKEN_USER,
                null_mut(),
                0,
                &mut needed,
            )
        };
        let error = io::Error::last_os_error();
        if result != 0 || error.raw_os_error() != Some(122) {
            return Err(contextual("size process token", error));
        }
        if !(size_of::<TokenUser>()..=MAX_SECURITY_BYTES).contains(&(needed as usize)) {
            return Err(invalid("process token size"));
        }
        // usize storage supplies pointer alignment for TOKEN_USER and its SID.
        let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        let capacity = buffer.len() * size_of::<usize>();
        checked(
            unsafe {
                GetTokenInformation(
                    token.as_raw_handle(),
                    TOKEN_USER,
                    buffer.as_mut_ptr().cast(),
                    capacity as u32,
                    &mut needed,
                )
            },
            "read process token",
        )?;
        if !(size_of::<TokenUser>()..=capacity).contains(&(needed as usize)) {
            return Err(invalid("process token result"));
        }
        // SAFETY: the successful query supplied at least an aligned TOKEN_USER.
        let sid = unsafe { (*(buffer.as_ptr() as *const TokenUser)).user.sid };
        let bytes = sid_length(sid, buffer.as_ptr().cast(), needed as usize)?;
        Ok(Self {
            _buffer: buffer,
            sid,
            bytes,
        })
    }
}

struct PrivateSecurity {
    user: CurrentUser,
    acl: Vec<u32>,
    descriptor: SecurityDescriptor,
}
impl PrivateSecurity {
    fn build() -> io::Result<Self> {
        let user = CurrentUser::load()?;
        let bytes = size_of::<Acl>() + size_of::<AllowedAce>() - size_of::<u32>() + user.bytes;
        let mut acl = vec![0u32; bytes.div_ceil(size_of::<u32>())];
        let pointer = acl.as_mut_ptr().cast();
        // SAFETY: aligned ACL storage has the exact advertised capacity; the validated SID stays owned.
        checked(
            unsafe { InitializeAcl(pointer, bytes as u32, 2) },
            "initialize DACL",
        )?;
        checked(
            unsafe { AddAccessAllowedAceEx(pointer, 2, 0, FILE_ALL_ACCESS, user.sid) },
            "build owner ACE",
        )?;
        validate_acl(pointer, acl.as_ptr().cast(), bytes, &user)?;
        let mut security = Self {
            user,
            acl,
            descriptor: SecurityDescriptor {
                revision: 0,
                reserved: 0,
                control: 0,
                owner: null_mut(),
                group: null_mut(),
                sacl: null_mut(),
                dacl: null_mut(),
            },
        };
        let descriptor = (&mut security.descriptor as *mut SecurityDescriptor).cast();
        // SAFETY: the descriptor has native layout; its SID/ACL point into owned heap allocations.
        checked(
            unsafe { InitializeSecurityDescriptor(descriptor, 1) },
            "initialize descriptor",
        )?;
        checked(
            unsafe { SetSecurityDescriptorOwner(descriptor, security.user.sid, 0) },
            "set descriptor owner",
        )?;
        checked(
            unsafe {
                SetSecurityDescriptorDacl(descriptor, 1, security.acl.as_mut_ptr().cast(), 0)
            },
            "set descriptor DACL",
        )?;
        checked(
            unsafe {
                SetSecurityDescriptorControl(descriptor, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
            },
            "protect descriptor",
        )?;
        if unsafe { IsValidSecurityDescriptor(descriptor) } == 0 {
            return Err(invalid("complete descriptor"));
        }
        Ok(security)
    }
}
fn validate_acl(
    acl: *mut Acl,
    base: *const u8,
    bytes: usize,
    user: &CurrentUser,
) -> io::Result<()> {
    if acl as usize & 3 != 0 || !contains(base, bytes, acl.cast(), size_of::<Acl>()) {
        return Err(invalid("DACL range"));
    }
    // SAFETY: the ACL header is inside the bounded allocation.
    let header = unsafe { read_unaligned(acl) };
    if header.revision != 2
        || header.count != 1
        || (header.bytes as usize) < size_of::<Acl>()
        || !contains(base, bytes, acl.cast(), header.bytes as usize)
    {
        return Err(invalid("one owner DACL"));
    }
    if unsafe { IsValidAcl(acl) } == 0 {
        return Err(invalid("DACL validity"));
    }
    let mut pointer = null_mut();
    // SAFETY: the OS-validated ACL contains exactly one ACE; output is range-checked before reads.
    checked(unsafe { GetAce(acl, 0, &mut pointer) }, "read owner ACE")?;
    if pointer as usize & 3 != 0
        || !contains(
            acl.cast(),
            header.bytes as usize,
            pointer,
            size_of::<AllowedAce>(),
        )
    {
        return Err(invalid("ACE range"));
    }
    let ace = unsafe { read_unaligned(pointer as *const AllowedAce) };
    let sid =
        unsafe { (pointer as *mut u8).add(size_of::<AllowedAce>() - size_of::<u32>()) }.cast();
    let length = sid_length(sid, acl.cast(), header.bytes as usize)?;
    if ace.header.kind != 0
        || ace.header.flags != 0
        || ace.mask != FILE_ALL_ACCESS
        || ace.header.bytes as usize != 8 + length
        || unsafe { EqualSid(sid, user.sid) } == 0
    {
        return Err(invalid("current user FullControl ACE"));
    }
    Ok(())
}

struct LocalDescriptor(*mut c_void);
impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        // SAFETY: GetSecurityInfo allocated this one descriptor with LocalAlloc; no interior pointer is freed.
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn verify_security(file: &File, user: &CurrentUser) -> io::Result<()> {
    let mut owner = null_mut();
    let mut dacl = null_mut();
    let mut raw = null_mut();
    // SAFETY: live file handle has READ_CONTROL; outputs are inspected only after ERROR_SUCCESS.
    status(
        unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                1,
                1 | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut raw,
            )
        },
        "read file security",
    )?;
    if raw.is_null() {
        return Err(invalid("security allocation"));
    }
    let descriptor = LocalDescriptor(raw);
    if unsafe { IsValidSecurityDescriptor(descriptor.0) } == 0 {
        return Err(invalid("file descriptor"));
    }
    let bytes = unsafe { GetSecurityDescriptorLength(descriptor.0) } as usize;
    if !(20..=MAX_SECURITY_BYTES).contains(&bytes) {
        return Err(invalid("file descriptor size"));
    }
    let mut control = 0;
    let mut revision = 0;
    checked(
        unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) },
        "read DACL protection",
    )?;
    if revision != 1
        || control & (SE_DACL_PRESENT | SE_DACL_PROTECTED) != (SE_DACL_PRESENT | SE_DACL_PROTECTED)
        || dacl.is_null()
    {
        return Err(invalid("protected nonnull DACL"));
    }
    sid_length(owner, descriptor.0.cast(), bytes)?;
    if unsafe { EqualSid(owner, user.sid) } == 0 {
        return Err(invalid("file owner identity"));
    }
    validate_acl(dacl, descriptor.0.cast(), bytes, user)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    volume: u32,
    index: [u32; 2],
    created: [u32; 2],
}
fn identity(file: &File, path: &Path, deadline: Instant) -> io::Result<Identity> {
    crate::snapshot::check_opened_path(file, path, deadline).map_err(|_| invalid("opened path"))?;
    let mut information = FileInformation::default();
    // SAFETY: the owned synchronous file handle and native output structure are valid.
    checked(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) },
        "read file identity",
    )?;
    if unsafe { GetFileType(file.as_raw_handle()) } != 1
        || information.attributes & (0x1 | 0x10 | 0x40 | 0x400) != 0
        || information.links != 1
        || information.volume == 0
        || information.index_low == 0 && information.index_high == 0
        || information.size_high != 0
        || information.size_low > 4096
    {
        return Err(invalid("ordinary single-link token identity"));
    }
    Ok(Identity {
        volume: information.volume,
        index: [information.index_low, information.index_high],
        created: information.created,
    })
}
fn checked_parents(path: &Path, deadline: Instant) -> io::Result<(PathBuf, Vec<File>)> {
    let components: Vec<_> = path.components().collect();
    if !matches!(components.first(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)))
        || !path.is_absolute()
        || path.file_name().is_none_or(|name| name != "token")
        || components.len() > 32
    {
        return Err(invalid("local absolute token path"));
    }
    for component in &components {
        if matches!(component, Component::ParentDir | Component::CurDir)
            || matches!(component, Component::Normal(name) if name.encode_wide().any(|unit| unit == 0 || unit == b':' as u16))
        {
            return Err(invalid("token path component"));
        }
    }
    let path = std::path::absolute(path).map_err(|error| contextual("absolute path", error))?;
    let mut parents: Vec<_> = path
        .parent()
        .ok_or_else(|| invalid("token parent"))?
        .ancestors()
        .collect();
    parents.reverse();
    let mut leases = Vec::with_capacity(parents.len());
    for parent in parents {
        leases.push(
            crate::snapshot::open_checked_writable_directory_identity(parent, deadline)
                .map_err(|_| invalid("ordinary parent identity"))?,
        );
    }
    Ok((path, leases))
}
fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "session token deadline expired",
        ));
    }
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context
            .check()
            .map_err(|error| contextual("operation context", error))?;
    }
    Ok(())
}

pub(super) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_before_apply(path, bytes, || Ok(()))
}
fn write_before_apply(
    path: &Path,
    bytes: &[u8],
    before_apply: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if bytes.len() > 4096 {
        return Err(invalid("token byte bound"));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let deadline = crate::operation_context::OperationContext::current()
        .map_or(deadline, |context| deadline.min(context.deadline()));
    check_deadline(deadline)?;
    let mut security = PrivateSecurity::build()?;
    let (path, _parents) = checked_parents(path, deadline)?;
    let mut name: Vec<_> = path.as_os_str().encode_wide().collect();
    name.push(0);
    let mut attributes = SecurityAttributes {
        bytes: size_of::<SecurityAttributes>() as u32,
        descriptor: (&mut security.descriptor as *mut SecurityDescriptor).cast(),
        inherit: 0,
    };
    check_deadline(deadline)?;
    // SAFETY: UTF-16 is terminated and all descriptor/SID/ACL backing storage stays live.
    // CREATE_NEW gives a fresh file its complete protected descriptor before any token byte exists.
    let mut raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            0x4000_0000 | 0x0002_0000 | 0x0004_0000 | 0x80,
            1,
            (&mut attributes as *mut SecurityAttributes).cast(),
            1,
            FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if raw == -1isize as Handle {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(80 | 183)) {
            return Err(contextual("create private file", error));
        }
        // OPEN_EXISTING never truncates. Its security descriptor is ignored by Windows, so
        // the existing handle must pass identity and ACL checks before any security/data mutation.
        raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                0x4000_0000 | 0x0002_0000 | 0x0004_0000 | 0x80,
                1,
                null_mut(),
                3,
                FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if raw == -1isize as Handle {
            return Err(contextual(
                "open existing private file",
                io::Error::last_os_error(),
            ));
        }
    }
    // SAFETY: exactly one successful CreateFileW handle is transferred to File and closed by it.
    let mut file = unsafe { File::from_raw_handle(raw) };
    let original = identity(&file, &path, deadline)?;
    verify_security(&file, &security.user)?;
    before_apply().map_err(|error| contextual("pre-apply check", error))?;
    check_deadline(deadline)?;
    // One handle-based call applies the already complete DACL and protection together.
    status(
        unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                1,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                security.acl.as_mut_ptr().cast(),
                null_mut(),
            )
        },
        "apply protected owner DACL",
    )?;
    verify_security(&file, &security.user)?;
    if identity(&file, &path, deadline)? != original {
        return Err(invalid("identity before write"));
    }
    check_deadline(deadline)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| contextual("seek", error))?;
    file.set_len(0)
        .map_err(|error| contextual("truncate", error))?;
    file.write_all(bytes)
        .map_err(|error| contextual("write", error))?;
    file.sync_all().map_err(|error| contextual("sync", error))?;
    if identity(&file, &path, deadline)? != original {
        return Err(invalid("identity after write"));
    }
    verify_security(&file, &security.user)?;
    check_deadline(deadline)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temporary {
        base: PathBuf,
        root: PathBuf,
    }
    impl Temporary {
        fn new() -> Self {
            let base = std::env::temp_dir();
            let root = base.join(format!(
                "aurum-session-token-r9-{}-{}",
                std::process::id(),
                crate::random::session_id()
            ));
            std::fs::create_dir(&root).unwrap();
            Self { base, root }
        }
        fn path(&self) -> PathBuf {
            self.root.join("token")
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) {
            use std::os::windows::fs::MetadataExt;
            if self.root.parent() == Some(self.base.as_path())
                && self.root.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with("aurum-session-token-r9-")
                })
                && std::fs::symlink_metadata(&self.root)
                    .is_ok_and(|meta| meta.is_dir() && meta.file_attributes() & 0x400 == 0)
            {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }
    fn security_bytes(file: &File) -> Vec<u8> {
        let mut raw = null_mut();
        // SAFETY: test owns a readable ordinary file; successful allocation is held until copied.
        status(
            unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    1,
                    1 | DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    &mut raw,
                )
            },
            "test security snapshot",
        )
        .unwrap();
        assert!(!raw.is_null(), "Security snapshot exists");
        let allocation = LocalDescriptor(raw);
        let bytes = unsafe { GetSecurityDescriptorLength(allocation.0) } as usize;
        assert!(
            (20..=MAX_SECURITY_BYTES).contains(&bytes),
            "Security snapshot bounded"
        );
        unsafe { std::slice::from_raw_parts(allocation.0.cast::<u8>(), bytes) }.to_vec()
    }
    #[test]
    fn new_token_has_protected_current_user_only_acl() {
        let temporary = Temporary::new();
        let session = crate::session::Session::create_in(&temporary.root, &temporary.root).unwrap();
        let path = session.directory.join("token");
        let file = File::open(&path).unwrap();
        let current = CurrentUser::load().unwrap();
        verify_security(&file, &current).unwrap();
        assert!(
            std::fs::read(&path).unwrap() == session.token.as_bytes(),
            "Private token content retained"
        );
    }
    #[test]
    fn repeated_token_write_preserves_private_acl_and_reopens() {
        let temporary = Temporary::new();
        write(&temporary.path(), b"first-private-value").unwrap();
        let first = security_bytes(&File::open(temporary.path()).unwrap());
        write(&temporary.path(), b"second").unwrap();
        let file = File::open(temporary.path()).unwrap();
        verify_security(&file, &CurrentUser::load().unwrap()).unwrap();
        assert!(
            security_bytes(&file) == first,
            "Repeated write preserves exact ACL descriptor"
        );
        assert!(
            std::fs::read(temporary.path()).unwrap() == b"second",
            "Repeated private write reopens without stale bytes"
        );
    }
    #[test]
    fn pre_apply_failure_preserves_existing_token_and_acl() {
        let temporary = Temporary::new();
        write(&temporary.path(), b"first-private-value").unwrap();
        let before = security_bytes(&File::open(temporary.path()).unwrap());
        let result = write_before_apply(&temporary.path(), b"replacement", || {
            Err(io::Error::other("controlled refusal"))
        });
        assert!(result.is_err(), "Injected pre-apply failure must propagate");
        assert!(
            std::fs::read(temporary.path()).unwrap() == b"first-private-value",
            "Failed preparation preserves existing bytes"
        );
        assert!(
            security_bytes(&File::open(temporary.path()).unwrap()) == before,
            "Failed preparation preserves existing ACL"
        );
    }
}
