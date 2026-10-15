//! Bounded tree journals. Deletion is a no-replace relocation, never a recursive
//! erase of a live user tree. Hashes deliberately omit timestamps and file IDs:
//! restoring a child through an older file journal must remain composable.
#[path = "file_zip.rs"]
mod archives;
#[path = "file_cleanup.rs"]
mod cleanup;
#[path = "file_io.rs"]
mod io_control;
#[path = "file_maintenance.rs"]
mod maintenance;
pub(crate) use maintenance::{
    cancel as cancel_maintenance, run as maintain_files, validate as validate_maintenance,
};
#[cfg(all(test, windows))]
#[path = "file_crash_tests.rs"]
mod crash_tests;
#[path = "file_cross.rs"]
mod cross;
#[path = "file_organize.rs"]
mod organizer;
#[path = "file_recovery.rs"]
mod recovery;
pub(crate) use recovery::revalidate as revalidate_undo;

#[cfg(all(test, windows))]
pub(crate) fn undo_crash_point(c: &Core) {
    crash_tests::at(c, "undo_namespace_restored");
}
pub(crate) use organizer::{
    directory_lease, file_snapshot, grouping_directory, recovery_paths, reserved_path,
};
#[cfg(all(test, windows))]
#[path = "file_tree_tests.rs"]
mod tests;
use crate::{runtime::Core, storage, undo};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_NODES: usize = 10_000;
const MAX_CHECKPOINT_BYTES: u64 = 1024 * 1024 * 1024;

pub struct Control {
    cancel: CancellationToken,
    deadline: Instant,
    roots: Mutex<Option<Vec<PathBuf>>>,
    maintenance_policy: Mutex<Option<Value>>,
    #[cfg(all(test, windows))]
    io_fault: Mutex<Option<io_control::Fault>>,
}
impl Default for Control {
    fn default() -> Self {
        Self::new(CancellationToken::new())
    }
}
impl Control {
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            deadline: Instant::now() + Duration::from_secs(85),
            roots: Mutex::new(None),
            maintenance_policy: Mutex::new(None),
            #[cfg(all(test, windows))]
            io_fault: Mutex::new(None),
        }
    }
    pub(super) fn bind_maintenance(&self, c: &Core, expected: &Value) -> Result<(), String> {
        *self
            .maintenance_policy
            .lock()
            .map_err(|_| "MAINTENANCE_LOCK")? = Some(expected.clone());
        self.check(c)
    }
    pub fn check(&self, c: &Core) -> Result<(), String> {
        if self.cancel.is_cancelled() {
            return Err("FILES_CANCELLED".into());
        }
        if Instant::now() >= self.deadline {
            return Err("FILES_DEADLINE".into());
        }
        if !c.enabled("files") {
            return Err("MODULE_DISABLED: files".into());
        }
        if self
            .maintenance_policy
            .lock()
            .map_err(|_| "MAINTENANCE_LOCK")?
            .as_ref()
            .is_some_and(|expected| *expected != maintenance::policy(c))
        {
            return Err("MAINTENANCE_POLICY_CHANGED".into());
        }
        let mut roots = self.roots.lock().map_err(|_| "FILES_SCOPE_LOCK")?;
        if roots.is_none() {
            c.normalize_scopes()?;
            if self.cancel.is_cancelled() {
                return Err("FILES_CANCELLED".into());
            }
            if Instant::now() >= self.deadline {
                return Err("FILES_DEADLINE".into());
            }
            if !c.enabled("files") {
                return Err("MODULE_DISABLED: files".into());
            }
        }
        let current = c.roots();
        if roots.as_ref().is_some_and(|saved| *saved != current) {
            return Err("FILES_SCOPE_CHANGED".into());
        }
        *roots = Some(current);
        Ok(())
    }
}

#[cfg(windows)]
fn open(path: &Path, rename: bool, parent: bool) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = std::fs::symlink_metadata(path)
        .map_err(|_| "TREE_METADATA")?
        .is_dir();
    // Do not follow a leaf reparse point. Ancestors are separately held open.
    // FILE_LIST_DIRECTORY activates share-access accounting. A metadata-only
    // directory handle does not prevent rename on the tested NTFS host.
    let access = 0x80 | if rename { 0x10000 } else { 0 } | if directory { 1 } else { 0x80000000 };
    let share = if parent || rename {
        if directory {
            3
        } else {
            1
        }
    } else {
        5
    };
    let file = OpenOptions::new()
        .access_mode(access)
        .share_mode(share)
        .custom_flags(0x02000000 | 0x00200000)
        .open(path)
        .map_err(|_| "TREE_LOCK_OR_ACCESS")?;
    use std::os::windows::fs::MetadataExt;
    if file
        .metadata()
        .map_err(|_| "TREE_METADATA")?
        .file_attributes()
        & 0x400
        != 0
    {
        return Err("PATH_REPARSE_DENIED".into());
    }
    Ok(file)
}
#[cfg(not(windows))]
fn open(_: &Path, _: bool, _: bool) -> Result<File, String> {
    Err("WINDOWS_11_REQUIRED".into())
}

fn parents(c: &Core, path: &Path) -> Result<Vec<File>, String> {
    c.path(path.to_str().ok_or("TREE_PATH_ENCODING")?, true)?;
    let ancestors: Vec<_> = path.parent().ok_or("TREE_PARENT")?.ancestors().collect();
    // Lease from the volume root inward, so no unleased ancestor can replace a
    // directory between opening a descendant and checking its ancestor.
    let mut leases = Vec::new();
    for ancestor in ancestors.iter().rev() {
        leases.push(open(ancestor, false, true)?);
    }
    leases.reverse();
    c.path(path.to_str().ok_or("TREE_PATH_ENCODING")?, true)?;
    Ok(leases)
}

#[cfg(windows)]
fn identity(file: &File) -> Result<String, String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|_| "TREE_IDENTITY")?;
    Ok(format!(
        "{:08x}:{:08x}{:08x}",
        info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
    ))
}
#[cfg(not(windows))]
fn identity(_: &File) -> Result<String, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(windows)]
fn volume(file: &File) -> Result<u32, String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|_| "TREE_IDENTITY")?;
    Ok(info.dwVolumeSerialNumber)
}
#[cfg(not(windows))]
fn volume(_: &File) -> Result<u32, String> {
    Err("WINDOWS_11_REQUIRED".into())
}

#[cfg(windows)]
fn rename(file: &File, parent: &File, destination: &Path) -> Result<(), String> {
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO},
    };
    if volume(file)? != volume(parent)? {
        return Err("TREE_CROSS_VOLUME_MOVE_PENDING".into());
    }
    // Win32's wrapper rejects a non-NULL RootDirectory on the tested host.
    // Destination ancestors remain leased against rename/delete, so an absolute
    // destination still resolves to the held parent and cannot overwrite.
    let leaf: Vec<u16> = destination.as_os_str().encode_wide().collect();
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = offset + (leaf.len() + 1) * 2;
    let mut aligned = vec![
        0u64;
        size.div_ceil(8)
            .max(std::mem::size_of::<FILE_RENAME_INFO>().div_ceil(8))
    ];
    let info = aligned.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // FILE_RENAME_INFO is variable-sized; the aligned backing allocation includes
    // every UTF-16 code unit. ReplaceIfExists remains FALSE.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = HANDLE::default();
        (*info).FileNameLength = (leaf.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(
            leaf.as_ptr(),
            (info as *mut u8).add(offset).cast::<u16>(),
            leaf.len(),
        );
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileRenameInfo,
            info.cast(),
            size as u32,
        )
    }
    .map_err(|e| format!("TREE_MOVE_FAILED_NO_REPLACE: 0x{:08x}", e.code().0 as u32))
}
#[cfg(not(windows))]
fn rename(_: &File, _: &File, _: &Path) -> Result<(), String> {
    Err("WINDOWS_11_REQUIRED".into())
}

#[cfg(windows)]
fn plain_streams(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::ERROR_HANDLE_EOF,
            Storage::FileSystem::{
                FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
                WIN32_FIND_STREAM_DATA,
            },
        },
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut data = WIN32_FIND_STREAM_DATA::default();
    let handle = match unsafe {
        FindFirstStreamW(
            PCWSTR(wide.as_ptr()),
            FindStreamInfoStandard,
            (&mut data as *mut WIN32_FIND_STREAM_DATA).cast(),
            None,
        )
    } {
        Ok(handle) => handle,
        Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_HANDLE_EOF.0) => {
            return Ok(())
        }
        Err(_) => return Err("TREE_STREAM_ENUMERATION".into()),
    };
    let result = loop {
        let len = data
            .cStreamName
            .iter()
            .position(|&v| v == 0)
            .unwrap_or(data.cStreamName.len());
        if String::from_utf16_lossy(&data.cStreamName[..len]) != "::$DATA" {
            break Err("TREE_ALTERNATE_STREAM_DENIED".into());
        }
        match unsafe { FindNextStreamW(handle, (&mut data as *mut WIN32_FIND_STREAM_DATA).cast()) }
        {
            Ok(()) => (),
            Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_HANDLE_EOF.0) => {
                break Ok(())
            }
            Err(_) => break Err("TREE_STREAM_ENUMERATION".into()),
        }
    };
    let _ = unsafe { FindClose(handle) };
    result
}
#[cfg(not(windows))]
fn plain_streams(_: &Path) -> Result<(), String> {
    Err("WINDOWS_11_REQUIRED".into())
}

struct Node {
    relative: PathBuf,
    file: File,
    directory: bool,
    readonly: bool,
}
pub struct Tree {
    root: File,
    nodes: Vec<Node>,
    pub digest: String,
    pub bytes: u64,
    identity: String,
    _parents: Vec<File>,
}

impl Tree {
    fn ownership(&self) -> Result<Value, String> {
        self.nodes.iter().map(|node| Ok(json!({"relative":node.relative.to_str().ok_or("TREE_PATH_ENCODING")?,"object_id":identity(&node.file)?}))).collect::<Result<Vec<Value>, String>>().map(|nodes| json!(nodes))
    }
    fn capture(c: &Core, path: &Path, mutate: bool, control: &Control) -> Result<Self, String> {
        control.check(c)?;
        let parents = parents(c, path)?;
        let root = open(path, mutate, false)?;
        let identity = identity(&root)?;
        let mut nodes = Vec::new();
        let mut stack = vec![PathBuf::new()];
        let mut total = 0u64;
        let mut digest = Sha256::new();
        digest.update(b"AlfredPet-tree-v1\0");
        while let Some(relative) = stack.pop() {
            control.check(c)?;
            if nodes.len() >= MAX_NODES || relative.components().count() > 64 {
                return Err("TREE_NODE_LIMIT".into());
            }
            let current = if relative.as_os_str().is_empty() {
                path.to_path_buf()
            } else {
                path.join(&relative)
            };
            c.path(current.to_str().ok_or("TREE_PATH_ENCODING")?, false)?;
            plain_streams(&current)?;
            let mut file = open(&current, false, false)?;
            let meta = file.metadata().map_err(|_| "TREE_METADATA")?;
            if !meta.is_dir() && !meta.is_file() {
                return Err("TREE_SPECIAL_FILE".into());
            }
            let directory = meta.is_dir();
            #[cfg(windows)]
            {
                use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
                use windows::Win32::{
                    Foundation::HANDLE,
                    Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
                };
                let mut info = BY_HANDLE_FILE_INFORMATION::default();
                unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
                    .map_err(|_| "TREE_IDENTITY")?;
                if !directory && info.nNumberOfLinks != 1 {
                    return Err("TREE_HARDLINK_DENIED".into());
                }
                let name: Vec<u16> = relative.as_os_str().encode_wide().collect();
                digest.update((name.len() as u64).to_le_bytes());
                for ch in name {
                    digest.update(ch.to_le_bytes());
                }
            }
            digest.update([u8::from(directory), u8::from(meta.permissions().readonly())]);
            if directory {
                let mut children = std::fs::read_dir(&current)
                    .map_err(|_| "TREE_DIRECTORY_READ")?
                    .map(|e| {
                        e.map(|e| relative.join(e.file_name()))
                            .map_err(|_| "TREE_ENTRY")
                    })
                    .take(MAX_NODES + 1)
                    .collect::<Result<Vec<_>, _>>()?;
                if children.len() > MAX_NODES {
                    return Err("TREE_NODE_LIMIT".into());
                }
                children.sort();
                stack.extend(children.into_iter().rev());
            } else {
                total = total.checked_add(meta.len()).ok_or("TREE_SIZE_LIMIT")?;
                if total > MAX_BYTES {
                    return Err("TREE_SIZE_LIMIT".into());
                }
                digest.update(meta.len().to_le_bytes());
                let mut count = 0u64;
                let mut buffer = [0u8; 65536];
                loop {
                    control.check(c)?;
                    let n = file.read(&mut buffer).map_err(|_| "TREE_READ")?;
                    if n == 0 {
                        break;
                    }
                    count += n as u64;
                    if count > meta.len() {
                        return Err("TREE_CHANGED_DURING_READ".into());
                    }
                    digest.update(&buffer[..n]);
                }
                if count != meta.len() {
                    return Err("TREE_CHANGED_DURING_READ".into());
                }
            }
            nodes.push(Node {
                relative,
                file,
                directory,
                readonly: meta.permissions().readonly(),
            });
        }
        Ok(Self {
            identity,
            root,
            nodes,
            digest: format!("{:x}", digest.finalize()),
            bytes: total,
            _parents: parents,
        })
    }
}

pub fn hash(c: &Core, path: &Path, control: &Control) -> Result<String, String> {
    Ok(Tree::capture(c, path, false, control)?.digest)
}

fn checkpoint(c: &Core, path: &Path, bytes: u64, control: &Control) -> Result<PathBuf, String> {
    check_quota(c, bytes, control)?;
    checkpoint_path(c, path)
}

fn checkpoint_path(c: &Core, path: &Path) -> Result<PathBuf, String> {
    checkpoint_path_avoiding(c, path, None)
}
fn checkpoint_path_avoiding(
    c: &Core,
    path: &Path,
    excluded: Option<&Path>,
) -> Result<PathBuf, String> {
    let source_parent = open(path.parent().ok_or("TREE_PARENT")?, false, true)?;
    let backup_root = c.path(
        c.root
            .join("backups")
            .to_str()
            .ok_or("TREE_PATH_ENCODING")?,
        false,
    )?;
    let backup_parent = open(&backup_root, false, true)?;
    let parent = if volume(&source_parent)? == volume(&backup_parent)?
        && !excluded.is_some_and(|source| backup_root.starts_with(source))
    {
        backup_root
    } else {
        let local = c.path(
            path.parent()
                .ok_or("TREE_PARENT")?
                .to_str()
                .ok_or("TREE_PATH_ENCODING")?,
            false,
        );
        match local {
            Ok(parent) => parent,
            Err(error) if error.starts_with("PATH_SCOPE") => {
                let mut candidate = None;
                for root in c.roots() {
                    let Some(raw) = root.to_str() else { continue };
                    let Ok(root) = c.path(raw, false) else {
                        continue;
                    };
                    if root.starts_with(path)
                        || excluded.is_some_and(|source| root.starts_with(source))
                        || !root.is_dir()
                    {
                        continue;
                    }
                    let Ok(handle) = open(&root, false, true) else {
                        continue;
                    };
                    if volume(&handle)? == volume(&source_parent)? {
                        candidate = Some(root);
                        break;
                    }
                }
                candidate.ok_or("TREE_CHECKPOINT_SCOPE: authorize a recovery folder on the source volume outside the moved root")?
            }
            Err(error) => return Err(error),
        }
    };
    if parent.starts_with(path) {
        return Err("TREE_CHECKPOINT_INSIDE_SOURCE".into());
    }
    c.path(
        parent
            .join(format!(".alfredpet-{}.checkpoint", storage::id()))
            .to_str()
            .ok_or("TREE_PATH_ENCODING")?,
        true,
    )
}

fn staging_path(
    c: &Core,
    destination: &Path,
    inside_existing: bool,
    excluded: &Path,
) -> Result<PathBuf, String> {
    let sibling = destination.parent().ok_or("TREE_PARENT")?;
    let parent = if c
        .path(sibling.to_str().ok_or("TREE_PATH_ENCODING")?, false)
        .is_ok()
    {
        sibling.to_path_buf()
    } else if inside_existing && destination.is_dir() {
        c.path(destination.to_str().ok_or("TREE_PATH_ENCODING")?, false)?
    } else {
        checkpoint_path_avoiding(c, destination, Some(excluded))?
            .parent()
            .ok_or("TREE_PARENT")?
            .to_path_buf()
    };
    c.path(
        parent
            .join(format!(".alfredpet-{}.staging", storage::id()))
            .to_str()
            .ok_or("TREE_PATH_ENCODING")?,
        true,
    )
}

fn check_quota(c: &Core, bytes: u64, control: &Control) -> Result<u64, String> {
    let mut retained = 0u64;
    for row in c
        .store
        .query(
            "SELECT checkpoint FROM undo_records WHERE checkpoint IS NOT NULL UNION SELECT staging AS checkpoint FROM undo_records WHERE staging IS NOT NULL UNION SELECT json_extract(metadata_json,'$.undo_target') AS checkpoint FROM undo_records WHERE json_extract(metadata_json,'$.undo_target') IS NOT NULL UNION SELECT json_extract(value,'$.work_path') AS checkpoint FROM file_cleanup_plans,json_each(snapshot_json,'$.entries') WHERE file_cleanup_plans.state!='completed'",
            &[],
        )?
        .as_array()
        .ok_or("UNDO_RECORDS")?
    {
        control.check(c)?;
        let p = Path::new(row["checkpoint"].as_str().ok_or("UNDO_CHECKPOINT")?);
        if p.exists() {
            retained = retained.saturating_add(Tree::capture(c, p, false, control)?.bytes);
        }
        if retained.saturating_add(bytes) > MAX_CHECKPOINT_BYTES {
            return Err("UNDO_CHECKPOINT_QUOTA: retained journals require review".into());
        }
    }
    if retained.saturating_add(bytes) > MAX_CHECKPOINT_BYTES {
        return Err("UNDO_CHECKPOINT_QUOTA".into());
    }
    Ok(retained)
}

fn scoped_root(c: &Core, path: &Path) -> bool {
    c.roots()
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .any(|p| p == path)
}

fn profile_root(c: &Core, path: &Path) -> bool {
    c.root
        .canonicalize()
        .is_ok_and(|root| root.starts_with(path))
        || ["temp", "backups", "plugins"].iter().any(|name| {
            c.root
                .join(name)
                .canonicalize()
                .is_ok_and(|root| root == path)
        })
}

fn source_parent(c: &Core, row: &Value, control: &Control) -> Result<Option<String>, String> {
    let data = metadata(row)?;
    let Some(expected) = data["source_parent_id"].as_str() else {
        return Ok(None);
    };
    control.check(c)?;
    let path = c.path(row["path"].as_str().ok_or("UNDO_PATH")?, true)?;
    if identity(&parents(c, &path)?[0])? != expected {
        return Err("UNDO_SOURCE_PARENT_REPLACED".into());
    }
    Ok(Some(expected.to_owned()))
}

fn relocate(
    c: &Core,
    source: &Path,
    destination: &Path,
    expected: &str,
    expected_id: &str,
    control: &Control,
) -> Result<(), String> {
    relocate_bound(c, source, destination, expected, expected_id, None, control)
}

fn relocate_bound(
    c: &Core,
    source: &Path,
    destination: &Path,
    expected: &str,
    expected_id: &str,
    expected_parent: Option<&str>,
    control: &Control,
) -> Result<(), String> {
    let mut tree = Tree::capture(c, source, true, control)?;
    if tree.identity != expected_id {
        return Err("UNDO_OBJECT_REPLACED".into());
    }
    if tree.digest != expected {
        return Err("UNDO_FILE_CHANGED: preserve later edits".into());
    }
    let destination_parents = parents(c, destination)?;
    if let Some(expected) = expected_parent {
        if identity(&destination_parents[0])? != expected {
            return Err("UNDO_SOURCE_PARENT_REPLACED".into());
        }
    }
    if destination.exists() {
        return Err("UNDO_DESTINATION_EXISTS".into());
    }
    control.check(c)?;
    c.path(source.to_str().ok_or("TREE_PATH_ENCODING")?, false)?;
    c.path(destination.to_str().ok_or("TREE_PATH_ENCODING")?, true)?;
    if hash(c, source, control)? != expected {
        return Err("TREE_CHANGED_BEFORE_MOVE".into());
    }
    // NTFS refuses a directory rename while descendant handles are open, even
    // with FILE_SHARE_DELETE. Release descendants for this short namespace step;
    // the root and every ancestor stay leased and the whole tree is rechecked.
    tree.nodes.clear();
    control.check(c)?;
    rename(&tree.root, &destination_parents[0], destination)?;
    // A new child can still appear while a directory is being renamed. Keep the
    // entire object and move it back on a mismatch; never delete its contents.
    let check = hash(c, destination, control);
    if !matches!(check, Ok(ref value) if value == expected) {
        let restored = rename(&tree.root, &tree._parents[0], source).is_ok();
        return Err(format!(
            "TREE_POST_MOVE_CHECK: restored={restored}; {}",
            check.err().unwrap_or_else(|| "tree changed".into())
        ));
    }
    Ok(())
}

pub fn mutate(
    c: &Core,
    action: &str,
    path: &Path,
    destination: Option<&Path>,
    control: &Control,
) -> Result<Value, String> {
    mutate_checked(c, action, path, destination, control, None)
}

pub(crate) fn mutate_checked(
    c: &Core,
    action: &str,
    path: &Path,
    destination: Option<&Path>,
    control: &Control,
    reviewed: Option<&Value>,
) -> Result<Value, String> {
    control.check(c)?;
    if action != "copy" && profile_root(c, path) {
        return Err("TREE_PROFILE_ROOT_PROTECTED".into());
    }
    let source = Tree::capture(c, path, false, control)?;
    if let Some(reviewed) = reviewed {
        if reviewed["object_id"] != source.identity
            || reviewed["sha256"] != source.digest
            || reviewed["root_id"] != identity(&source._parents[0])?
        {
            return Err("ORGANIZE_SOURCE_CHANGED".into());
        }
    }
    if matches!(action, "move" | "rename") {
        let target = destination.ok_or("TREE_DESTINATION")?;
        let leased = parents(c, target)?;
        if let Some(reviewed) = reviewed {
            if reviewed["destination_parent_id"] != identity(&leased[0])? {
                return Err("ORGANIZE_DESTINATION_PARENT_CHANGED".into());
            }
        }
        if target.exists() {
            return Err("DESTINATION_EXISTS".into());
        }
        if volume(&source.root)? != volume(&leased[0])? {
            return cross::mutate(c, path, target, source, control);
        }
    }
    let (kind, target) = if action == "delete" {
        ("tree_delete", checkpoint(c, path, source.bytes, control)?)
    } else {
        let dest = destination.ok_or("TREE_DESTINATION")?;
        if dest.starts_with(path) || path.starts_with(dest) {
            return Err("TREE_OVERLAPPING_PATHS".into());
        }
        if dest.exists() {
            return Err("DESTINATION_EXISTS".into());
        }
        (
            if action == "copy" {
                "tree_copy"
            } else {
                "tree_move"
            },
            dest.to_path_buf(),
        )
    };
    let retained = if kind == "tree_delete" {
        Some(target.clone())
    } else if kind == "tree_copy" {
        check_quota(c, source.bytes, control)?;
        Some(checkpoint_path_avoiding(c, &target, Some(path))?)
    } else {
        None
    };
    let token = undo::prepare_tree(
        c,
        kind,
        path,
        if kind == "tree_delete" {
            None
        } else {
            Some(&target)
        },
        retained.as_deref(),
        &source.digest,
    )?;
    let digest = source.digest.clone();
    let source_id = source.identity.clone();
    if action != "copy" {
        let mut data = json!({"source_parent_id":identity(&source._parents[0])?,"scope_root":scoped_root(c,path)});
        if kind == "tree_delete" {
            data["checkpoint_id"] = json!(source_id);
            data["checkpoint_nodes"] = source.ownership()?;
        }
        save_metadata(c, &token, &data)?;
    }
    let result = if action == "copy" {
        copy(c, &source, path, &target, &token, control)
    } else {
        c.store.execute(
            "UPDATE undo_records SET object_id=? WHERE id=?",
            &[json!(source_id), json!(token)],
        )?;
        #[cfg(all(test, windows))]
        crash_tests::at(c, "tree_journal_prepared");
        drop(source);
        relocate_bound(
            c,
            path,
            &target,
            &digest,
            &source_id,
            reviewed.and_then(|value| value["destination_parent_id"].as_str()),
            control,
        )
    };
    if let Err(error) = result {
        return Err(format!("{error}; recovery_undo_id={token}"));
    }
    #[cfg(all(test, windows))]
    crash_tests::at(c, "tree_namespace_published");
    undo::commit(c, &token).map_err(|error| format!("{error}; recovery_undo_id={token}"))?;
    Ok(
        json!({"original":path,"path":target,"checkpoint":retained,"undo_available":true,"undo_token":token}),
    )
}

#[cfg(windows)]
fn pinned_read(path: &Path) -> Result<File, String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let directory = std::fs::symlink_metadata(path)
        .map_err(|_| "TREE_METADATA")?
        .is_dir();
    let file = OpenOptions::new()
        .access_mode(if directory { 0x81 } else { 0x80000000 })
        .share_mode(1)
        .custom_flags(0x02000000 | 0x00200000)
        .open(path)
        .map_err(|_| "COPY_SOURCE_LOCK_OR_ACCESS")?;
    if file
        .metadata()
        .map_err(|_| "TREE_METADATA")?
        .file_attributes()
        & 0x400
        != 0
    {
        return Err("PATH_REPARSE_DENIED".into());
    }
    Ok(file)
}
#[cfg(not(windows))]
fn pinned_read(_: &Path) -> Result<File, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
fn pin_source(
    c: &Core,
    source: &Tree,
    path: &Path,
    control: &Control,
) -> Result<Vec<File>, String> {
    let mut pins = Vec::with_capacity(source.nodes.len());
    for node in &source.nodes {
        control.check(c)?;
        let current = if node.relative.as_os_str().is_empty() {
            path.to_owned()
        } else {
            path.join(&node.relative)
        };
        let pin = pinned_read(&current)?;
        if identity(&pin)? != identity(&node.file)? {
            return Err("COPY_SOURCE_REPLACED".into());
        }
        pins.push(pin);
    }
    Ok(pins)
}
fn copy(
    c: &Core,
    source: &Tree,
    original: &Path,
    destination: &Path,
    token: &str,
    control: &Control,
) -> Result<(), String> {
    let _source_pins = pin_source(c, source, original, control)?;
    let _parents = parents(c, destination)?;
    let stage = staging_path(c, destination, false, original)?;
    c.store.execute(
        "UPDATE undo_records SET staging=? WHERE id=? AND state='prepared'",
        &[json!(stage), json!(token)],
    )?;
    // Staging is owned, exclusive and never published until source and output
    // have matching bounded manifests. Failed stages are retained for recovery.
    for node in &source.nodes {
        control.check(c)?;
        let out = if node.relative.as_os_str().is_empty() {
            stage.clone()
        } else {
            stage.join(&node.relative)
        };
        c.path(out.to_str().ok_or("TREE_PATH_ENCODING")?, true)?;
        if node.directory {
            std::fs::create_dir(&out).map_err(|_| "TREE_COPY_DIRECTORY")?;
            if node.relative.as_os_str().is_empty() {
                let mut data = journal_metadata(c, token)?;
                data["stage_id"] = json!(identity(&open(&out, false, false)?)?);
                save_metadata(c, token, &data)?;
                #[cfg(all(test, windows))]
                crash_tests::at(c, "copy_stage_created");
            }
        } else {
            let mut input = node.file.try_clone().map_err(|_| "TREE_COPY_READ")?;
            input
                .seek(SeekFrom::Start(0))
                .map_err(|_| "TREE_COPY_READ")?;
            let mut output = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&out)
                .map_err(|_| "TREE_COPY_CREATE")?;
            if node.relative.as_os_str().is_empty() {
                let mut data = journal_metadata(c, token)?;
                data["stage_id"] = json!(identity(&output)?);
                save_metadata(c, token, &data)?;
                #[cfg(all(test, windows))]
                crash_tests::at(c, "copy_stage_created");
            }
            let mut buffer = [0u8; 65536];
            loop {
                control.check(c)?;
                let n = io_control::read(
                    &mut input,
                    &mut buffer,
                    io_control::Point::CopyRead,
                    control,
                )?;
                if n == 0 {
                    break;
                }
                io_control::write_all(
                    &mut output,
                    &buffer[..n],
                    io_control::Point::CopyWrite,
                    control,
                )?;
            }
            io_control::flush(&output, io_control::Point::CopyFlush, control)?;
        }
        let mut permissions = std::fs::metadata(&out)
            .map_err(|_| "TREE_COPY_METADATA")?
            .permissions();
        permissions.set_readonly(node.readonly);
        std::fs::set_permissions(&out, permissions).map_err(|_| "TREE_COPY_PERMISSIONS")?;
    }
    let staged = Tree::capture(c, &stage, false, control)?;
    if staged.identity
        != journal_metadata(c, token)?["stage_id"]
            .as_str()
            .ok_or("COPY_STAGE_ID")?
    {
        return Err("COPY_STAGE_OBJECT_REPLACED".into());
    }
    #[cfg(all(test, windows))]
    crash_tests::at(c, "copy_before_validation");
    let current_source = Tree::capture(c, original, false, control)?;
    if current_source.identity != source.identity
        || current_source.ownership()? != source.ownership()?
        || current_source.digest != source.digest
        || staged.digest != source.digest
    {
        return Err("TREE_COPY_CHANGED".into());
    }
    let stage_id = staged.identity.clone();
    let mut data = journal_metadata(c, token)?;
    data["result_nodes"] = staged.ownership()?;
    save_metadata(c, token, &data)?;
    drop(staged);
    c.store.execute(
        "UPDATE undo_records SET object_id=? WHERE id=? AND state='prepared'",
        &[json!(stage_id), json!(token)],
    )?;
    #[cfg(all(test, windows))]
    crash_tests::at(c, "copy_before_publication");
    relocate(c, &stage, destination, &source.digest, &stage_id, control)?;
    c.store.execute(
        "UPDATE undo_records SET staging=NULL WHERE id=?",
        &[json!(token)],
    )?;
    Ok(())
}

pub fn expected_state(c: &Core, row: &Value, control: &Control) -> Result<(), String> {
    if row["kind"] == "tree_cross_move" {
        return cross::expected_state(c, row, control);
    }
    let original = c.path(row["path"].as_str().ok_or("UNDO_PATH")?, true)?;
    source_parent(c, row, control)?;
    let expected = row["expected_hash"].as_str().ok_or("UNDO_HASH")?;
    let current = if row["kind"] == "tree_delete" {
        if original.exists() {
            return Err("UNDO_DESTINATION_EXISTS".into());
        }
        c.path(row["checkpoint"].as_str().ok_or("UNDO_CHECKPOINT")?, false)?
    } else {
        c.path(
            row["destination"].as_str().ok_or("UNDO_DESTINATION")?,
            false,
        )?
    };
    let current_tree = Tree::capture(c, &current, false, control)?;
    if current_tree.identity != row["object_id"].as_str().ok_or("UNDO_OBJECT_ID_MISSING")? {
        return Err("UNDO_OBJECT_REPLACED".into());
    }
    if current_tree.digest != expected {
        return Err("UNDO_FILE_CHANGED: preserve later edits".into());
    }
    if row["kind"] == "tree_move" && original.exists() {
        return Err("UNDO_DESTINATION_EXISTS".into());
    }
    Ok(())
}

pub fn restore(c: &Core, row: &Value, control: &Control) -> Result<(), String> {
    if row["kind"] == "tree_cross_move" {
        return cross::restore(c, row, control);
    }
    expected_state(c, row, control)?;
    let original = c.path(row["path"].as_str().ok_or("UNDO_PATH")?, true)?;
    let (source, dest) = match row["kind"].as_str() {
        Some("tree_delete") => (
            c.path(row["checkpoint"].as_str().ok_or("UNDO_CHECKPOINT")?, false)?,
            original,
        ),
        Some("tree_move") => (
            c.path(
                row["destination"].as_str().ok_or("UNDO_DESTINATION")?,
                false,
            )?,
            original,
        ),
        Some("tree_copy") => (
            c.path(
                row["destination"].as_str().ok_or("UNDO_DESTINATION")?,
                false,
            )?,
            c.path(row["checkpoint"].as_str().ok_or("UNDO_CHECKPOINT")?, true)?,
        ),
        _ => return Err("UNDO_KIND".into()),
    };
    if row["kind"] == "tree_copy" {
        check_quota(c, Tree::capture(c, &source, false, control)?.bytes, control)?;
    }
    let parent = if row["kind"] == "tree_copy" {
        None
    } else {
        source_parent(c, row, control)?
    };
    relocate_bound(
        c,
        &source,
        &dest,
        row["expected_hash"].as_str().ok_or("UNDO_HASH")?,
        row["object_id"].as_str().ok_or("UNDO_OBJECT_ID_MISSING")?,
        parent.as_deref(),
        control,
    )
}

fn metadata(row: &Value) -> Result<Value, String> {
    let data: Value = serde_json::from_str(
        row["metadata_json"]
            .as_str()
            .ok_or("FILE_METADATA_JOURNAL")?,
    )
    .map_err(|_| "FILE_METADATA_JOURNAL")?;
    if !data.is_object() {
        return Err("FILE_METADATA_JOURNAL".into());
    }
    Ok(data)
}
fn journal_metadata(c: &Core, id: &str) -> Result<Value, String> {
    metadata(
        &c.store.query(
            "SELECT metadata_json FROM undo_records WHERE id=?",
            &[json!(id)],
        )?[0],
    )
}
fn save_metadata(c: &Core, id: &str, data: &Value) -> Result<(), String> {
    c.store.execute(
        "UPDATE undo_records SET metadata_json=?,updated_at=? WHERE id=?",
        &[json!(data.to_string()), json!(storage::now()), json!(id)],
    )?;
    Ok(())
}
pub fn recover_cross(c: &Core, row: &Value) -> Result<(), String> {
    cross::recover(c, row)
}
pub fn archive(
    c: &Core,
    action: &str,
    path: &Path,
    destination: &Path,
    control: &Control,
) -> Result<Value, String> {
    match action {
        "compress" => archives::compress(c, path, destination, control),
        "extract" => archives::extract(c, path, destination, control),
        _ => Err("ZIP_ACTION_UNKNOWN".into()),
    }
}
pub fn archive_status(c: &Core, id: &str, offset: u64) -> Result<Value, String> {
    archives::status(c, id, offset)
}
pub fn cleanup(c: &Core, body: &Value, control: &Control) -> Result<Value, String> {
    cleanup::dispatch(c, body, control)
}
pub fn cleanup_review(c: &Core, body: &Value) -> Result<Value, String> {
    cleanup::review(c, body)
}

fn raw_hash(c: &Core, file: &mut File, control: &Control) -> Result<String, String> {
    hash_file(file, || control.check(c))
}
fn hash_file(file: &mut File, check: impl FnMut() -> Result<(), String>) -> Result<String, String> {
    hash_file_limit(file, 32 * 1024 * 1024, check)
}
fn hash_file_limit(
    file: &mut File,
    limit: u64,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<String, String> {
    file.seek(SeekFrom::Start(0)).map_err(|_| "FILE_SEEK")?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        check()?;
        let n = file.read(&mut buffer).map_err(|_| "FILE_READ")?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limit {
            return Err("FILE_SIZE_LIMIT".into());
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
#[cfg(windows)]
fn write_handle(path: &Path, create: bool) -> Result<File, String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .share_mode(1)
        .custom_flags(0x00200000);
    if create {
        options.create_new(true);
    }
    let file = options
        .open(path)
        .map_err(|_| "FILE_WRITE_LOCK_OR_EXISTS")?;
    let meta = file.metadata().map_err(|_| "FILE_METADATA")?;
    if !meta.is_file() || meta.file_attributes() & 0x400 != 0 {
        return Err("PATH_REPARSE_DENIED".into());
    }
    plain_streams(path)?;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|_| "TREE_IDENTITY")?;
    if info.nNumberOfLinks != 1 {
        return Err("TREE_HARDLINK_DENIED".into());
    }
    Ok(file)
}
#[cfg(not(windows))]
fn write_handle(_: &Path, _: bool) -> Result<File, String> {
    Err("WINDOWS_11_REQUIRED".into())
}

pub fn write(c: &Core, path: &Path, text: &str, control: &Control) -> Result<Value, String> {
    if text.len() > 64000 {
        return Err("FILE_TEXT_LIMIT".into());
    }
    control.check(c)?;
    let _parents = parents(c, path)?;
    let existed = path.exists();
    let expected = format!("{:x}", Sha256::digest(text.as_bytes()));
    let backup = if existed {
        Some(checkpoint(
            c,
            path,
            std::fs::metadata(path).map_err(|_| "FILE_METADATA")?.len(),
            control,
        )?)
    } else {
        None
    };
    let _backup_parents = backup.as_ref().map(|p| parents(c, p)).transpose()?;
    let token = undo::prepare_tree(c, "write", path, None, backup.as_deref(), &expected)?;
    save_metadata(
        c,
        &token,
        &json!({"source_parent_id":identity(&_parents[0])?,"existed":existed}),
    )?;
    let result = (|| {
        let mut target = write_handle(path, !existed)?;
        let target_id = identity(&target)?;
        c.store.execute(
            "UPDATE undo_records SET object_id=? WHERE id=?",
            &[json!(target_id), json!(token)],
        )?;
        let mut attempted = false;
        let outcome = (|| {
            if let Some(backup) = &backup {
                let digest = raw_hash(c, &mut target, control)?;
                let mut data = journal_metadata(c, &token)?;
                data["original_hash"] = json!(digest);
                save_metadata(c, &token, &data)?;
                target.seek(SeekFrom::Start(0)).map_err(|_| "FILE_SEEK")?;
                let mut saved = write_handle(backup, true)?;
                data["checkpoint_id"] = json!(identity(&saved)?);
                save_metadata(c, &token, &data)?;
                let mut buffer = [0u8; 65536];
                loop {
                    control.check(c)?;
                    let n = io_control::read(
                        &mut target,
                        &mut buffer,
                        io_control::Point::CheckpointRead,
                        control,
                    )?;
                    if n == 0 {
                        break;
                    }
                    io_control::write_all(
                        &mut saved,
                        &buffer[..n],
                        io_control::Point::CheckpointWrite,
                        control,
                    )?;
                }
                io_control::flush(&saved, io_control::Point::CheckpointFlush, control)?;
                if raw_hash(c, &mut saved, control)? != digest {
                    return Err("CHECKPOINT_READBACK".into());
                }
                c.store.execute(
                    "UPDATE undo_records SET checkpoint_hash=? WHERE id=?",
                    &[json!(digest), json!(token)],
                )?;
                data["checkpoint_complete"] = json!(true);
                save_metadata(c, &token, &data)?;
            }
            control.check(c)?;
            c.path(path.to_str().ok_or("TREE_PATH_ENCODING")?, true)?;
            target.seek(SeekFrom::Start(0)).map_err(|_| "FILE_SEEK")?;
            let mut data = journal_metadata(c, &token)?;
            data["write_started"] = json!(true);
            save_metadata(c, &token, &data)?;
            #[cfg(all(test, windows))]
            crash_tests::at(c, "write_before_truncate");
            control.check(c)?;
            attempted = true;
            target.set_len(0).map_err(|_| "FILE_WRITE")?;
            io_control::write_all(
                &mut target,
                text.as_bytes(),
                io_control::Point::Write,
                control,
            )?;
            io_control::flush(&target, io_control::Point::Flush, control)?;
            if raw_hash(c, &mut target, control)? != expected {
                return Err("FILE_WRITE_READBACK".into());
            }
            undo::commit(c, &token)?;
            Ok::<(), String>(())
        })();
        if let Err(error) = &outcome {
            // Keep the exclusive write/rename lease while observing a failure.
            // Never adopt bytes read after releasing it to a different writer.
            #[cfg(all(test, windows))]
            crash_tests::at(c, "write_failure_before_observation");
            let observed = hash_file_limit(&mut target, MAX_BYTES, || Ok(()));
            let mut data = journal_metadata(c, &token)?;
            data["write_failure"] = json!({"mutation_attempted":attempted,"created":!existed,"object_id":target_id,"observed_hash":observed.ok(),"error":error});
            if save_metadata(c, &token, &data).is_err() {
                return Err(format!("{error}; FILE_FAILURE_NOT_JOURNALED"));
            }
            #[cfg(all(test, windows))]
            crash_tests::at(c, "write_failure_observed");
        }
        outcome
    })();
    if let Err(error) = result {
        let stored = c.store.execute(
            "UPDATE undo_records SET state='blocked',updated_at=? WHERE id=? AND state='prepared'",
            &[json!(storage::now()), json!(token)],
        );
        return Err(format!(
            "{error}; recovery_undo_id={token}{}",
            if stored.is_err() {
                "; FILE_FAILURE_STATE_NOT_JOURNALED"
            } else {
                ""
            }
        ));
    }
    Ok(json!({"path":path,"checkpoint":backup,"sha256":expected,"undo_token":token}))
}

fn write_expected_hash(row: &Value) -> Result<String, String> {
    let data = metadata(row)?;
    let failure = if data["undo_started"] == true && data["undo_failure"].is_object() {
        &data["undo_failure"]
    } else {
        &data["write_failure"]
    };
    if failure.is_object() {
        if failure["object_id"] != row["object_id"] {
            return Err("UNDO_FAILURE_ID_MISSING".into());
        }
        if !row["checkpoint"].is_null()
            && (data["checkpoint_complete"] != true
                || data["original_hash"] != row["checkpoint_hash"])
        {
            return Err("UNDO_CHECKPOINT_INCOMPLETE".into());
        }
        let hash = failure["observed_hash"]
            .as_str()
            .ok_or("UNDO_FAILURE_HASH_MISSING")?;
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err("UNDO_FAILURE_HASH_MISSING".into());
        }
        Ok(hash.to_owned())
    } else {
        Ok(row["expected_hash"].as_str().ok_or("UNDO_HASH")?.to_owned())
    }
}

pub fn restore_write(c: &Core, row: &Value, control: &Control) -> Result<(), String> {
    control.check(c)?;
    let original = c.path(row["path"].as_str().ok_or("UNDO_PATH")?, true)?;
    let _parents = parents(c, &original)?;
    source_parent(c, row, control)?;
    let expected = write_expected_hash(row)?;
    if row["checkpoint"].is_null() {
        let mut target = open(&original, true, false)?;
        if let Some(saved) = row["object_id"].as_str() {
            if identity(&target)? != saved {
                return Err("UNDO_OBJECT_REPLACED".into());
            }
        }
        if raw_hash(c, &mut target, control)? != expected {
            return Err("UNDO_FILE_CHANGED: preserve later edits".into());
        }
        control.check(c)?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{
                Foundation::HANDLE,
                Storage::FileSystem::{
                    FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
                },
            };
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            unsafe {
                SetFileInformationByHandle(
                    HANDLE(target.as_raw_handle()),
                    FileDispositionInfo,
                    (&info as *const FILE_DISPOSITION_INFO).cast(),
                    std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            }
            .map_err(|_| "UNDO_REMOVE")?;
        }
        return Ok(());
    }
    let checkpoint = c.path(row["checkpoint"].as_str().ok_or("UNDO_CHECKPOINT")?, false)?;
    let _checkpoint_parents = parents(c, &checkpoint)?;
    let mut backup = open(&checkpoint, false, false)?;
    if let Some(saved) = metadata(row)?["checkpoint_id"].as_str() {
        if identity(&backup)? != saved {
            return Err("UNDO_CHECKPOINT_REPLACED".into());
        }
    }
    let saved_hash = row["checkpoint_hash"]
        .as_str()
        .ok_or("UNDO_CHECKPOINT_HASH")?;
    if raw_hash(c, &mut backup, control)? != saved_hash {
        return Err("UNDO_CHECKPOINT_CHANGED".into());
    }
    let mut target = write_handle(&original, false)?;
    if let Some(saved) = row["object_id"].as_str() {
        if identity(&target)? != saved {
            return Err("UNDO_OBJECT_REPLACED".into());
        }
    }
    if raw_hash(c, &mut target, control)? != expected {
        return Err("UNDO_FILE_CHANGED: preserve later edits".into());
    }
    control.check(c)?;
    let restored = (|| {
        backup.seek(SeekFrom::Start(0)).map_err(|_| "UNDO_SEEK")?;
        target.seek(SeekFrom::Start(0)).map_err(|_| "UNDO_SEEK")?;
        target.set_len(0).map_err(|_| "UNDO_RESTORE")?;
        // Once mutation begins, finish the bounded restoration under the same
        // exclusive handle even if the caller drops its future.
        let mut buffer = [0u8; 65536];
        loop {
            let n = backup.read(&mut buffer).map_err(|_| "UNDO_RESTORE")?;
            if n == 0 {
                break;
            }
            io_control::write_all(
                &mut target,
                &buffer[..n],
                io_control::Point::UndoRestore,
                control,
            )?;
        }
        io_control::flush(&target, io_control::Point::UndoFlush, control)?;
        if hash_file(&mut target, || Ok(()))? != saved_hash {
            return Err("UNDO_RESTORE_READBACK".into());
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = &restored {
        let blocked = c.store.execute(
            "UPDATE undo_records SET state='blocked',updated_at=? WHERE id=?",
            &[json!(storage::now()), row["id"].clone()],
        );
        let observed = hash_file_limit(&mut target, MAX_BYTES, || Ok(()));
        let mut data = metadata(row)?;
        data["checkpoint_complete"] = json!(true);
        data["original_hash"] = json!(saved_hash);
        data["undo_failure"] =
            json!({"object_id":identity(&target)?,"observed_hash":observed.ok(),"error":error});
        if save_metadata(c, row["id"].as_str().ok_or("UNDO_ID")?, &data).is_err() {
            return Err(format!("{error}; UNDO_FAILURE_NOT_JOURNALED"));
        }
        blocked?;
    }
    restored
}

pub fn validate_retention(value: &Value) -> Result<(), String> {
    cleanup::validate_retention(value)
}
pub fn retention(c: &Core, control: &Control) -> Result<Value, String> {
    cleanup::retention(c, control)
}
