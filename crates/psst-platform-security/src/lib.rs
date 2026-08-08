#![deny(unsafe_op_in_unsafe_fn)]

/// Returns the effective Unix user identity used for filesystem authority checks.
#[cfg(unix)]
#[must_use]
pub fn effective_uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[cfg(windows)]
use std::{fs::File, io};

#[cfg(unix)]
use std::{
    fs::File,
    io::{self, Read},
};

/// Opens and pins a local directory with relative-child creation/replacement rights.
///
/// # Errors
/// Returns the native operating-system error when the directory cannot be pinned safely.
#[cfg(windows)]
pub fn open_pinned_directory(path: &std::path::Path) -> io::Result<File> {
    use std::{
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            CreateFileW, FILE_ADD_FILE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, SYNCHRONIZE,
        },
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_ADD_FILE | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_handle(handle) })
    }
}

/// Opens one simple child name relative to an already-pinned Windows directory.
///
/// # Errors
/// Rejects non-simple names and returns the native error without following a reparse point.
#[cfg(windows)]
#[allow(clippy::too_many_lines)]
pub fn open_relative_file(
    directory: &File,
    name: &std::ffi::OsStr,
    delete_access: bool,
) -> io::Result<Option<File>> {
    use std::{
        mem,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle},
        },
        ptr,
    };
    use windows_sys::Win32::Foundation::{GENERIC_READ, HANDLE};
    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: HANDLE,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut core::ffi::c_void,
        security_quality_of_service: *mut core::ffi::c_void,
    }
    #[repr(C)]
    struct IoStatusBlock {
        status: isize,
        information: usize,
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtCreateFile(
            file: *mut HANDLE,
            access: u32,
            attributes: *mut ObjectAttributes,
            status: *mut IoStatusBlock,
            allocation: *const i64,
            file_attributes: u32,
            share_access: u32,
            disposition: u32,
            options: u32,
            ea_buffer: *const core::ffi::c_void,
            ea_length: u32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }
    let wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty() || wide.iter().any(|unit| matches!(*unit, 0 | 47 | 92)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "relative name required",
        ));
    }
    let byte_len = u16::try_from(wide.len().saturating_mul(2))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name too long"))?;
    let mut name = wide;
    let mut unicode = UnicodeString {
        length: byte_len,
        maximum_length: byte_len,
        buffer: name.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: u32::try_from(mem::size_of::<ObjectAttributes>()).unwrap_or(u32::MAX),
        root_directory: directory.as_raw_handle(),
        object_name: &raw mut unicode,
        attributes: 0x40,
        security_descriptor: ptr::null_mut(),
        security_quality_of_service: ptr::null_mut(),
    };
    let mut status_block = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut handle: HANDLE = ptr::null_mut();
    let access = GENERIC_READ | 0x0010_0000 | if delete_access { 0x0001_0000 } else { 0 };
    let share = 1 | 2 | if delete_access { 0 } else { 4 };
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            access,
            &raw mut attributes,
            &raw mut status_block,
            ptr::null(),
            0,
            share,
            1,
            0x40 | 0x20 | 0x0020_0000,
            ptr::null(),
            0,
        )
    };
    if status == i32::from_ne_bytes(0xC000_0034_u32.to_ne_bytes()) {
        return Ok(None);
    }
    if status < 0 {
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(
            i32::try_from(code).unwrap_or(i32::MAX),
        ));
    }
    if handle.is_null() {
        return Err(io::Error::other(
            "native relative open returned a null handle",
        ));
    }
    Ok(Some(unsafe { File::from_raw_handle(handle) }))
}

/// Creates one write-through file relative to a pinned directory with a protected SID-only DACL.
///
/// # Errors
/// Rejects non-simple names and returns the native create/security error.
#[cfg(windows)]
#[allow(clippy::too_many_lines)]
pub fn create_relative_restricted_file(
    directory: &File,
    name: &std::ffi::OsStr,
    sid: &str,
) -> io::Result<File> {
    use std::{
        mem,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle},
        },
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, LocalFree},
        Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        },
    };
    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: HANDLE,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut core::ffi::c_void,
        security_quality_of_service: *mut core::ffi::c_void,
    }
    #[repr(C)]
    struct IoStatusBlock {
        status: isize,
        information: usize,
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtCreateFile(
            file: *mut HANDLE,
            access: u32,
            attributes: *mut ObjectAttributes,
            status: *mut IoStatusBlock,
            allocation: *const i64,
            file_attributes: u32,
            share_access: u32,
            disposition: u32,
            options: u32,
            ea_buffer: *const core::ffi::c_void,
            ea_length: u32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }
    let mut name: Vec<u16> = name.encode_wide().collect();
    if name.is_empty() || name.iter().any(|unit| matches!(*unit, 0 | 47 | 92)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "relative name required",
        ));
    }
    let byte_len = u16::try_from(name.len().saturating_mul(2))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name too long"))?;
    let sddl: Vec<u16> = format!("D:P(A;;FA;;;{sid})\0").encode_utf16().collect();
    let mut descriptor = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut unicode = UnicodeString {
        length: byte_len,
        maximum_length: byte_len,
        buffer: name.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: u32::try_from(mem::size_of::<ObjectAttributes>()).unwrap_or(u32::MAX),
        root_directory: directory.as_raw_handle(),
        object_name: &raw mut unicode,
        attributes: 0x40,
        security_descriptor: descriptor,
        security_quality_of_service: ptr::null_mut(),
    };
    let mut status_block = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut handle: HANDLE = ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            GENERIC_READ | GENERIC_WRITE | 0x0001_0000 | 0x0010_0000,
            &raw mut attributes,
            &raw mut status_block,
            ptr::null(),
            0x80,
            1 | 2,
            2,
            0x40 | 0x20 | 0x0020_0000 | 0x2,
            ptr::null(),
            0,
        )
    };
    unsafe { LocalFree(descriptor) };
    if status < 0 {
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(
            i32::try_from(code).unwrap_or(i32::MAX),
        ));
    }
    if handle.is_null() {
        return Err(io::Error::other(
            "native relative create returned a null handle",
        ));
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Verifies that a stable path resides on the local fixed NTFS durability baseline.
///
/// # Errors
/// Fails closed when the filesystem cannot be identified exactly as NTFS.
#[cfg(windows)]
pub fn verify_local_ntfs_path(path: &std::path::Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW,
    };
    let input: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut root = [0_u16; 512];
    if unsafe {
        GetVolumePathNameW(
            input.as_ptr(),
            root.as_mut_ptr(),
            u32::try_from(root.len()).unwrap_or(u32::MAX),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if unsafe { GetDriveTypeW(root.as_ptr()) } != 3 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "non-fixed storage is outside the leave journal durability baseline",
        ));
    }
    let mut filesystem = [0_u16; 32];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem.as_mut_ptr(),
            u32::try_from(filesystem.len()).unwrap_or(u32::MAX),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = filesystem
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(filesystem.len());
    if String::from_utf16_lossy(&filesystem[..length]).eq_ignore_ascii_case("NTFS") {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "leave journal requires local NTFS durability semantics",
        ))
    }
}

/// Returns the current process token's SID without launching a helper process.
///
/// # Errors
/// Returns the native token/query/conversion error.
#[cfg(windows)]
pub fn current_process_sid() -> io::Result<String> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{Authorization::ConvertSidToStringSidW, TOKEN_USER, TokenUser},
    };
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> HANDLE;
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: HANDLE, access: u32, token: *mut HANDLE) -> i32;
        fn GetTokenInformation(
            token: HANDLE,
            class: i32,
            information: *mut core::ffi::c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }
    let mut token: HANDLE = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), 0x0008, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut required = 0_u32;
        unsafe {
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &raw mut required);
        }
        if required == 0 {
            return Err(io::Error::last_os_error());
        }
        let required_usize = usize::try_from(required).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "token information too large")
        })?;
        let mut buffer = vec![0_usize; required_usize.div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                required,
                &raw mut required,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut text = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut length = 0_usize;
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
        unsafe { LocalFree(text.cast()) };
        Ok(value)
    })();
    unsafe { CloseHandle(token) };
    result
}

/// Fills a buffer using the Windows system-preferred cryptographic RNG.
///
/// # Errors
/// Returns an error when the native RNG rejects the request.
#[cfg(windows)]
pub fn fill_secure_random(output: &mut [u8]) -> io::Result<()> {
    #[link(name = "bcrypt")]
    unsafe extern "system" {
        fn BCryptGenRandom(
            algorithm: *mut core::ffi::c_void,
            buffer: *mut u8,
            length: u32,
            flags: u32,
        ) -> i32;
    }
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            output.as_mut_ptr(),
            u32::try_from(output.len())
                .map_err(|_| io::Error::other("random request too large"))?,
            0x0000_0002,
        )
    };
    if status < 0 {
        Err(io::Error::other("secure randomness unavailable"))
    } else {
        Ok(())
    }
}

/// Fills a buffer using the operating system's cryptographic random source.
///
/// # Errors
/// Returns an error when the operating system cannot provide secure randomness.
#[cfg(unix)]
pub fn fill_secure_random(output: &mut [u8]) -> io::Result<()> {
    File::open("/dev/urandom")?.read_exact(output)
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::fill_secure_random;

    #[test]
    fn secure_random_accepts_empty_and_nonempty_buffers() {
        fill_secure_random(&mut []).unwrap();

        let mut output = [0_u8; 32];
        fill_secure_random(&mut output).unwrap();
    }
}

/// Creates a new Windows file whose initial DACL is already protected and SID-only.
///
/// # Errors
/// Returns the native operating-system error when descriptor creation or file creation fails.
#[cfg(windows)]
pub fn create_restricted_file(path: &std::path::Path, sid: &str) -> io::Result<File> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        },
    };
    let sddl: Vec<u16> = format!("D:P(A;;FA;;;{sid})\0").encode_utf16().collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let attributes_len = u32::try_from(mem::size_of::<SECURITY_ATTRIBUTES>()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "security attributes too large")
    })?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: attributes_len,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE | 0x0004_0000 | 0x0001_0000,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &raw mut attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    unsafe { LocalFree(descriptor) };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_handle(handle) })
    }
}

/// Verifies the exact protected SID-only DACL through the held file handle.
///
/// # Errors
/// Returns permission denied unless the held handle has the exact expected protected DACL.
#[cfg(windows)]
#[allow(clippy::too_many_lines)]
pub fn verify_restricted_file(file: &File, sid: &str) -> io::Result<()> {
    use std::{os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
            Authorization::{ConvertStringSidToSidW, GetSecurityInfo, SE_FILE_OBJECT},
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
            GetSecurityDescriptorControl, PROTECTED_DACL_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR, PSID, SE_DACL_PRESENT, SE_DACL_PROTECTED,
        },
        Storage::FileSystem::FILE_ALL_ACCESS,
    };
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(i32::MAX),
        ));
    }
    let mut control = 0_u16;
    let mut revision = 0_u32;
    let mut size = ACL_SIZE_INFORMATION::default();
    let mut ace = ptr::null_mut();
    let sid_text: Vec<u16> = sid.encode_utf16().chain(Some(0)).collect();
    let mut expected_sid: PSID = ptr::null_mut();
    let control_ok =
        unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) }
            != 0;
    let acl_ok = !dacl.is_null()
        && unsafe {
            GetAclInformation(
                dacl,
                (&raw mut size).cast(),
                u32::try_from(std::mem::size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(u32::MAX),
                AclSizeInformation,
            )
        } != 0;
    let single_ace_ok =
        acl_ok && size.AceCount == 1 && unsafe { GetAce(dacl, 0, &raw mut ace) } != 0;
    let sid_ok = unsafe { ConvertStringSidToSidW(sid_text.as_ptr(), &raw mut expected_sid) } != 0;
    let mut ace_type = u8::MAX;
    let mut ace_flags = u8::MAX;
    let mut access_mask = 0_u32;
    let mut equal_sid = false;
    if single_ace_ok && sid_ok {
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        let ace_sid = unsafe { (&raw const (*allowed).SidStart).cast_mut().cast() };
        ace_type = unsafe { (*allowed).Header.AceType };
        ace_flags = unsafe { (*allowed).Header.AceFlags };
        access_mask = unsafe { (*allowed).Mask };
        equal_sid = unsafe { EqualSid(ace_sid, expected_sid) != 0 };
    }
    let failure = if !control_ok {
        "control_query"
    } else if control & SE_DACL_PRESENT == 0 {
        "dacl_not_present"
    } else if control & SE_DACL_PROTECTED == 0 {
        "dacl_not_protected"
    } else if dacl.is_null() {
        "null_dacl"
    } else if !acl_ok {
        "acl_query"
    } else if size.AceCount != 1 {
        "ace_count"
    } else if !single_ace_ok {
        "ace_query"
    } else if ace_type != 0 {
        "ace_type"
    } else if ace_flags != 0 {
        "ace_flags"
    } else if access_mask != FILE_ALL_ACCESS {
        "access_mask"
    } else if !sid_ok {
        "sid_parse"
    } else if !equal_sid {
        "sid_mismatch"
    } else {
        "none"
    };
    unsafe {
        LocalFree(expected_sid.cast());
        LocalFree(descriptor);
    }
    if failure == "none" {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "credential handle DACL is unsafe ({failure}; control={control:#06x}; ace_count={}; ace_type={ace_type}; ace_flags={ace_flags:#04x}; access_mask={access_mask:#010x})",
                size.AceCount
            ),
        ))
    }
}

/// Marks the exact held file object for deletion without reopening its pathname.
///
/// # Errors
/// Returns the native operating-system error when the handle lacks delete access or the
/// filesystem cannot apply delete disposition.
#[cfg(windows)]
pub fn delete_held_file(file: &File) -> io::Result<()> {
    use std::{mem, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    let size = u32::try_from(mem::size_of::<FILE_DISPOSITION_INFO>())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "disposition too large"))?;
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            size,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Replaces a Windows file handle's DACL with one protected full-control ACE.
///
/// # Errors
/// Returns the native operating-system error when conversion or application fails.
#[cfg(windows)]
pub fn restrict_file_to_sid(file: &File, sid: &str) -> io::Result<()> {
    use std::{os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            SetKernelObjectSecurity,
        },
    };
    let sddl: Vec<u16> = format!("D:P(A;;FA;;;{sid})\0").encode_utf16().collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the string is NUL-terminated and the out pointer is valid.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the file stays open and descriptor stays allocated for this call.
    let applied = unsafe {
        SetKernelObjectSecurity(
            file.as_raw_handle(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    // SAFETY: this is the exact allocation returned by the conversion function.
    unsafe { LocalFree(descriptor) };
    if applied == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Atomically replaces `target` by renaming the already-open Windows file handle.
///
/// # Errors
/// Returns the native operating-system error when handle-relative replacement fails.
#[cfg(windows)]
pub fn replace_file_by_handle(
    file: &File,
    directory: &File,
    target: &std::ffi::OsStr,
) -> io::Result<()> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
        ptr,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_RENAME_INFO;
    #[repr(C)]
    struct IoStatusBlock {
        status: isize,
        information: usize,
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSetInformationFile(
            handle: *mut core::ffi::c_void,
            status: *mut IoStatusBlock,
            info: *const core::ffi::c_void,
            length: u32,
            class: i32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: i32) -> u32;
    }
    let name: Vec<u16> = target.encode_wide().collect();
    let offset = mem::offset_of!(FILE_RENAME_INFO, FileName);
    let buffer_len = offset + name.len() * mem::size_of::<u16>();
    let mut buffer = vec![0_usize; buffer_len.div_ceil(mem::size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: the allocation is sized through the flexible filename member and
    // remains live throughout the native call.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = true;
        (*info).RootDirectory = directory.as_raw_handle();
        (*info).FileNameLength = u32::try_from(name.len() * 2)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "target path too long"))?;
        ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
    }
    let mut status_block = IoStatusBlock {
        status: 0,
        information: 0,
    };
    // SAFETY: `info` points to the initialized FILE_RENAME_INFORMATION-compatible buffer.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &raw mut status_block,
            info.cast(),
            u32::try_from(buffer_len).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "rename buffer too long")
            })?,
            10,
        )
    };
    if status < 0 {
        let code = unsafe { RtlNtStatusToDosError(status) };
        Err(io::Error::from_raw_os_error(
            i32::try_from(code).unwrap_or(i32::MAX),
        ))
    } else {
        Ok(())
    }
}
