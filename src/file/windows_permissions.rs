//! Check directory creation rights without the backup semantics of its handle.

use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    io,
    mem::size_of,
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{self, NonNull},
};

use windows_sys::Win32::{
    Foundation::{
        LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED,
        ERROR_NO_TOKEN, ERROR_SUCCESS, LUID,
    },
    Security::{
        AccessCheck,
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        DuplicateToken, LookupPrivilegeValueA, PrivilegeCheck, SecurityIdentification,
        DACL_SECURITY_INFORMATION, GENERIC_MAPPING, GROUP_SECURITY_INFORMATION,
        LABEL_SECURITY_INFORMATION, LUID_AND_ATTRIBUTES, OWNER_SECURITY_INFORMATION, PRIVILEGE_SET,
        SE_PRIVILEGE_ENABLED, TOKEN_DUPLICATE, TOKEN_QUERY,
    },
    Storage::FileSystem::{
        GetVolumeInformationByHandleW, FILE_ADD_FILE, FILE_ALL_ACCESS, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, READ_CONTROL,
    },
    System::Threading::{GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken},
};

/// A LocalAlloc-owned descriptor returned by GetSecurityInfo.
struct SecurityDescriptor(NonNull<c_void>);

impl SecurityDescriptor {
    fn from_directory(directory: &File) -> io::Result<Self> {
        let mut descriptor = ptr::null_mut();
        // SAFETY: the live directory handle has READ_CONTROL. The out pointer
        // receives one allocation, kept intact until its LocalFree in Drop.
        let error = unsafe {
            GetSecurityInfo(
                directory.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | GROUP_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | LABEL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if error != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        NonNull::new(descriptor)
            .map(Self)
            .ok_or_else(|| io::Error::other("GetSecurityInfo returned no descriptor"))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: this is the allocation GetSecurityInfo returned, freed once.
        unsafe { LocalFree(self.0.as_ptr()) };
    }
}

/// Snapshot the effective token without changing the thread or process token.
fn effective_token() -> io::Result<OwnedHandle> {
    let mut token = ptr::null_mut();
    // SAFETY: the current-thread pseudo-handle is live; token is writable.
    // OpenAsSelf only authorizes opening the token; the token remains that
    // of the impersonated thread, rather than the process's identity.
    if unsafe {
        OpenThreadToken(
            GetCurrentThread(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            1,
            &mut token,
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
            return Err(error);
        }
        // SAFETY: no thread token exists, so the process token is effective.
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE,
                &mut token,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: a successful token open returns one owned, valid handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut duplicate = ptr::null_mut();
    // SAFETY: the source token stays live. AccessCheck needs an impersonation
    // token; identification level is sufficient and does not elevate an
    // existing identification-level thread token.
    if unsafe {
        DuplicateToken(
            token.as_raw_handle(),
            SecurityIdentification,
            &mut duplicate,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateToken returned a distinct owned handle, closed by Drop.
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicate) })
}

fn backup_privileges_enabled(token: &OwnedHandle) -> io::Result<bool> {
    for name in [c"SeBackupPrivilege", c"SeRestorePrivilege"] {
        let mut luid = LUID::default();
        // SAFETY: this is a static NUL-terminated privilege name; luid is writable.
        if unsafe { LookupPrivilegeValueA(ptr::null(), name.as_ptr().cast(), &mut luid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut required = PRIVILEGE_SET {
            PrivilegeCount: 1,
            Control: 0, // Any listed privilege; this set contains exactly one.
            Privilege: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let mut enabled = 0;
        // SAFETY: token is live and queryable; both outputs stay writable.
        if unsafe { PrivilegeCheck(token.as_raw_handle(), &mut required, &mut enabled) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if enabled != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn volume_has_persistent_acls(directory: &File) -> io::Result<bool> {
    const FILE_PERSISTENT_ACLS: u32 = 0x0000_0008;
    let mut flags = 0;
    // SAFETY: directory stays live; unrequested outputs are null, flags is writable.
    if unsafe {
        GetVolumeInformationByHandleW(
            directory.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut flags,
            ptr::null_mut(),
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(flags & FILE_PERSISTENT_ACLS != 0)
}

fn directory_allows_new_file(directory: &Path) -> io::Result<bool> {
    let token = effective_token()?;
    let privileged = backup_privileges_enabled(&token)?;
    // The same held handle identifies the reparse target and its descriptor.
    // BACKUP_SEMANTICS is required to open directories, but enabled restore
    // privileges can make that open succeed despite a FILE_ADD_FILE denial.
    let directory = match OpenOptions::new()
        .access_mode(FILE_ADD_FILE | READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory)
    {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            // A missing READ_CONTROL right need not prevent child creation.
            // The narrower probe proves that only when backup/restore
            // privileges cannot bypass its ordinary access check.
            return match OpenOptions::new()
                .access_mode(FILE_ADD_FILE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(directory)
            {
                Ok(_) if !privileged => Ok(true),
                Ok(_) => Err(error),
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(false),
                Err(error) => Err(error),
            };
        }
        Err(error) => return Err(error),
    };
    // Without enabled backup/restore privileges the kernel's successful open
    // proves both the DACL and mandatory integrity label permit child creation.
    if !privileged {
        return Ok(true);
    }
    // Backup intent can bypass the DACL. Evaluate the complete descriptor,
    // including its mandatory label, independently of those privileges.
    let descriptor = match SecurityDescriptor::from_directory(&directory) {
        Ok(descriptor) => descriptor,
        Err(error)
            if matches!(error.raw_os_error(), Some(code) if code == ERROR_NOT_SUPPORTED as i32 || code == ERROR_INVALID_FUNCTION as i32)
                && !volume_has_persistent_acls(&directory)? =>
        {
            // The native add-file open succeeded and the volume explicitly
            // reports no persistent ACLs. No DACL denial can be bypassed here.
            return Ok(true);
        }
        Err(error) => return Err(error),
    };
    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ,
        GenericWrite: FILE_GENERIC_WRITE,
        GenericExecute: FILE_GENERIC_EXECUTE,
        GenericAll: FILE_ALL_ACCESS,
    };
    // Vec supplies aligned, owned storage for the variable-length Windows
    // PRIVILEGE_SET; any retry grows it before exposing the pointer again.
    let mut privileges = vec![PRIVILEGE_SET::default()];
    loop {
        let capacity_bytes = privileges.len() * size_of::<PRIVILEGE_SET>();
        let mut bytes = u32::try_from(capacity_bytes)
            .map_err(|_| io::Error::other("AccessCheck privilege buffer exceeds Windows limits"))?;
        let mut granted = 0;
        let mut allowed = 0;
        // SAFETY: descriptor/token stay live; every output points to writable
        // storage of the stated size. FILE_ADD_FILE contains no generic bits.
        // This checks the DACL and mandatory label without the handle's backup intent.
        if unsafe {
            AccessCheck(
                descriptor.0.as_ptr(),
                token.as_raw_handle(),
                FILE_ADD_FILE,
                &mapping,
                privileges.as_mut_ptr(),
                &mut bytes,
                &mut granted,
                &mut allowed,
            )
        } != 0
        {
            return Ok(allowed != 0);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
            || bytes as usize <= capacity_bytes
        {
            return Err(error);
        }
        privileges.resize(
            (bytes as usize).div_ceil(size_of::<PRIVILEGE_SET>()),
            PRIVILEGE_SET::default(),
        );
    }
}

pub(super) fn check_directory(directory: &Path) -> io::Result<bool> {
    directory_allows_new_file(directory).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot check the recording directory {} permissions: {error}",
                directory.display()
            ),
        )
    })
}
