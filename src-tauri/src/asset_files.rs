//! Handles and protected current-user ACLs for managed capture/attachment assets.
use crate::runtime::Core;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    };
    use windows::{
        core::{BOOL, PCWSTR, PWSTR},
        Win32::{
            Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL},
            Security::{Authorization::*, *},
            Storage::FileSystem::*,
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };
    struct Local(*mut core::ffi::c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                let _ = unsafe { LocalFree(Some(HLOCAL(self.0))) };
            }
        }
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    fn sid_string(sid: PSID) -> Result<String, String> {
        let mut text = PWSTR::null();
        unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(|_| "ASSET_SID")?;
        let _owned = Local(text.0.cast());
        unsafe { text.to_string() }.map_err(|_| "ASSET_SID".into())
    }
    pub(crate) fn user_sid() -> Result<String, String> {
        let mut token = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .map_err(|_| "ASSET_TOKEN")?;
        let token = Token(token);
        let mut size = 0;
        let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut size) };
        if size == 0 || size > 65536 {
            return Err("ASSET_TOKEN".into());
        }
        // u64 storage supplies the alignment required by TOKEN_USER on Windows x64.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                size,
                &mut size,
            )
        }
        .map_err(|_| "ASSET_TOKEN")?;
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        sid_string(user.User.Sid)
    }
    struct Descriptor(Local);
    impl Descriptor {
        fn new(directory: bool) -> Result<Self, String> {
            let sid = user_sid()?;
            let flags = if directory { "OICI" } else { "" };
            let sddl = format!("O:{sid}D:P(A;{flags};FA;;;{sid})");
            let wide = sddl.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let mut sd = PSECURITY_DESCRIPTOR::default();
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(wide.as_ptr()),
                    SDDL_REVISION_1,
                    &mut sd,
                    None,
                )
            }
            .map_err(|_| "ASSET_DACL_CREATE")?;
            Ok(Self(Local(sd.0)))
        }
        fn attributes(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.0 .0,
                bInheritHandle: BOOL(0),
            }
        }
    }
    pub fn private(file: &File) -> Result<(), String> {
        let mut owner = PSID::default();
        let mut acl = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            GetSecurityInfo(
                HANDLE(file.as_raw_handle()),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                Some(&mut owner),
                None,
                Some(&mut acl),
                None,
                Some(&mut sd),
            )
        }
        .ok()
        .map_err(|_| "ASSET_DACL_READ")?;
        let _owned = Local(sd.0);
        let mut control = 0;
        let mut revision = 0;
        unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) }
            .map_err(|_| "ASSET_DACL_READ")?;
        if acl.is_null()
            || control & SE_DACL_PROTECTED.0 == 0
            || unsafe { (*acl).AceCount } != 1
            || sid_string(owner)? != user_sid()?
        {
            return Err("ASSET_DACL_NOT_PRIVATE".into());
        }
        let mut ace = std::ptr::null_mut();
        unsafe { GetAce(acl, 0, &mut ace) }.map_err(|_| "ASSET_DACL_READ")?;
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        if allowed.Header.AceType != 0
            || allowed.Mask != 0x001f01ff
            || sid_string(PSID((&allowed.SidStart as *const u32).cast_mut().cast()))? != user_sid()?
        {
            return Err("ASSET_DACL_NOT_PRIVATE".into());
        }
        Ok(())
    }
    pub fn identity(file: &File) -> Result<String, String> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|_| "ASSET_IDENTITY")?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            || (!file.metadata().map_err(|_| "ASSET_METADATA")?.is_dir()
                && info.nNumberOfLinks != 1)
        {
            return Err("ASSET_LINK_DENIED".into());
        }
        Ok(format!(
            "{:08x}:{:08x}{:08x}",
            info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
        ))
    }
    pub fn directory(path: &Path) -> Result<File, String> {
        let file = std::fs::OpenOptions::new()
            .access_mode(0x20081)
            .share_mode(3)
            .custom_flags(0x02200000)
            .open(path)
            .map_err(|_| "ASSET_DIRECTORY_LOCK_OR_ACCESS")?;
        if !file.metadata().map_err(|_| "ASSET_METADATA")?.is_dir() {
            return Err("ASSET_DIRECTORY".into());
        }
        identity(&file)?;
        Ok(file)
    }
    pub fn open(path: &Path, erase: bool) -> Result<File, String> {
        let file = std::fs::OpenOptions::new()
            .access_mode(0x80020080 | if erase { 0x10000 } else { 0 })
            .share_mode(1)
            .custom_flags(0x00200000)
            .open(path)
            .map_err(|_| "ASSET_LOCK_OR_ACCESS")?;
        if !file.metadata().map_err(|_| "ASSET_METADATA")?.is_file() {
            return Err("ASSET_NOT_FILE".into());
        }
        identity(&file)?;
        Ok(file)
    }
    pub fn create(path: &Path) -> Result<File, String> {
        let descriptor = Descriptor::new(false)?;
        let attributes = descriptor.attributes();
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                0xc0030000,
                FILE_SHARE_READ,
                Some(&attributes),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(|_| "ASSET_CREATE_NEW")?;
        let file = unsafe { File::from_raw_handle(handle.0) };
        identity(&file)?;
        private(&file)?;
        Ok(file)
    }
    pub fn create_directory(path: &Path) -> Result<(), String> {
        let descriptor = Descriptor::new(true)?;
        let attributes = descriptor.attributes();
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(&attributes)) }
            .map_err(|_| "ASSET_DIRECTORY_CREATE_NEW".into())
    }
    pub fn lock(path: &Path) -> Result<File, String> {
        let file = std::fs::OpenOptions::new()
            .access_mode(0xc0020080)
            .share_mode(0)
            .custom_flags(0x00200000)
            .open(path)
            .map_err(|_| "ASSET_PROFILE_BUSY_OR_LOCK_CHANGED")?;
        identity(&file)?;
        private(&file)?;
        Ok(file)
    }
    pub fn erase(file: &File) -> Result<(), String> {
        let info = FILE_DISPOSITION_INFO_EX {
            Flags: FILE_DISPOSITION_FLAG_DELETE,
        };
        unsafe {
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileDispositionInfoEx,
                (&info as *const FILE_DISPOSITION_INFO_EX).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        }
        .map_err(|_| "ASSET_ERASE_HANDLE".into())
    }
}
#[cfg(windows)]
pub(crate) use win::user_sid;
#[cfg(windows)]
pub use win::{create, create_directory, directory, erase, identity, lock, open, private};
#[cfg(not(windows))]
pub fn identity(_: &File) -> Result<String, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
pub fn private(_: &File) -> Result<(), String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
pub fn directory(_: &Path) -> Result<File, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
pub fn open(_: &Path, _: bool) -> Result<File, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
pub fn create(_: &Path) -> Result<File, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
pub fn create_directory(_: &Path) -> Result<(), String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
pub fn lock(_: &Path) -> Result<File, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
pub fn erase(_: &File) -> Result<(), String> {
    Err("WINDOWS_11_REQUIRED".into())
}
pub fn parents(core: &Core, path: &Path) -> Result<Vec<File>, String> {
    core.path(path.to_str().ok_or("ASSET_PATH_ENCODING")?, true)?;
    let ancestors = path
        .parent()
        .ok_or("ASSET_PARENT")?
        .ancestors()
        .collect::<Vec<_>>();
    let mut held = Vec::new();
    for parent in ancestors.into_iter().rev() {
        held.push(directory(parent)?);
    }
    core.path(path.to_str().ok_or("ASSET_PATH_ENCODING")?, true)?;
    Ok(held)
}
pub fn read(file: &File, limit: u64) -> Result<Vec<u8>, String> {
    let mut reader = file.try_clone().map_err(|_| "ASSET_READ")?;
    reader.seek(SeekFrom::Start(0)).map_err(|_| "ASSET_READ")?;
    if reader.metadata().map_err(|_| "ASSET_METADATA")?.len() > limit {
        return Err("ASSET_BYTE_LIMIT".into());
    }
    let mut data = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut data)
        .map_err(|_| "ASSET_READ")?;
    if data.len() as u64 > limit {
        return Err("ASSET_BYTE_LIMIT".into());
    }
    Ok(data)
}
