//! ZIPs are completely validated in an owned stage before publishing. Existing
//! folders are merged by publishing disjoint new objects, each with its own
//! durable tree journal. Existing files are never replaced.
use super::*;
use std::{collections::BTreeMap, io};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const MAX_METADATA: u64 = 16 * 1024 * 1024;
const MAX_ZIP64_END: usize = 65_536;

struct CheckedIo<'a> {
    file: File,
    c: &'a Core,
    control: &'a Control,
    read_bytes: u64,
    output_limit: u64,
    metadata_ends: Option<(u64, Option<u64>)>,
    metadata_phase: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ambiguous_end: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl CheckedIo<'_> {
    fn check(&self) -> io::Result<()> {
        self.control.check(self.c).map_err(io::Error::other)
    }
    fn guard_metadata(&mut self) -> io::Result<()> {
        if let Some((end, zip64)) = self.metadata_ends.filter(|_| {
            self.metadata_phase
                .load(std::sync::atomic::Ordering::Relaxed)
        }) {
            let position = self.file.stream_position()?;
            let mut signature = [0; 4];
            let n = self.file.read(&mut signature)?;
            self.file.seek(SeekFrom::Start(position))?;
            if n == 4
                && ((signature == [0x50, 0x4b, 0x05, 0x06] && position != end)
                    || (signature == [0x50, 0x4b, 0x06, 0x06] && Some(position) != zip64))
            {
                self.ambiguous_end
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return Err(io::Error::other("ZIP_AMBIGUOUS_END"));
            }
        }
        Ok(())
    }
}
impl Read for CheckedIo<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        self.guard_metadata()?;
        let n = self.file.read(bytes)?;
        self.read_bytes = self.read_bytes.saturating_add(n as u64);
        let limit = if self
            .metadata_phase
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            MAX_METADATA * 2
        } else {
            MAX_BYTES * 4
        };
        if self.read_bytes > limit {
            return Err(io::Error::other("ZIP_INPUT_READ_LIMIT"));
        }
        Ok(n)
    }
}
impl Write for CheckedIo<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check()?;
        if self
            .file
            .stream_position()?
            .saturating_add(bytes.len() as u64)
            > MAX_BYTES
        {
            return Err(io::Error::other("ZIP_OUTPUT_SIZE_LIMIT"));
        }
        if self
            .file
            .stream_position()?
            .saturating_add(bytes.len() as u64)
            > self.output_limit
        {
            return Err(io::Error::other("UNDO_CHECKPOINT_QUOTA"));
        }
        self.file.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.file.flush()
    }
}
impl Seek for CheckedIo<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        let p = self.file.seek(position)?;
        if p > MAX_BYTES {
            return Err(io::Error::other("ZIP_OFFSET_LIMIT"));
        }
        Ok(p)
    }
}
fn checked<'a>(file: File, c: &'a Core, control: &'a Control) -> CheckedIo<'a> {
    CheckedIo {
        file,
        c,
        control,
        read_bytes: 0,
        output_limit: MAX_BYTES,
        metadata_ends: None,
        metadata_phase: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ambiguous_end: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}
fn zip_error(error: impl std::fmt::Display) -> String {
    format!("ZIP_INVALID_OR_IO: {error}")
}
fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

// Bound metadata allocations before calling the library's central directory
// parser. PKWARE APPNOTE 4.3.14-4.3.16 defines these fixed-size trailers.
fn trailer(reader: &mut CheckedIo<'_>) -> Result<usize, String> {
    let length = reader.seek(SeekFrom::End(0)).map_err(zip_error)?;
    if length < 22 {
        return Err("ZIP_INVALID".into());
    }
    let take = (length as usize).min(22 + 65_535 + 20 + MAX_ZIP64_END);
    reader
        .seek(SeekFrom::End(-(take as i64)))
        .map_err(zip_error)?;
    let mut tail = vec![0; take];
    reader.read_exact(&mut tail).map_err(zip_error)?;
    let end = (0..=take - 22)
        .rev()
        .find(|&i| {
            tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]
                && i + 22 + u16_at(&tail, i + 20) as usize == take
        })
        .ok_or("ZIP_INVALID_END")?;
    let base = length - take as u64;
    let mut zip64_end = None;
    let (count, size) = if end >= 20 && tail[end - 20..end - 16] == [0x50, 0x4b, 0x06, 0x07] {
        let locator = end - 20;
        if u32_at(&tail, locator + 4) != 0 || u32_at(&tail, locator + 16) != 1 {
            return Err("ZIP_MULTIDISK_UNSUPPORTED".into());
        }
        let record = (0..=locator.saturating_sub(56))
            .rev()
            .find(|&i| {
                tail[i..i + 4] == [0x50, 0x4b, 0x06, 0x06]
                    && u64_at(&tail, i + 4).checked_add(i as u64 + 12) == Some(locator as u64)
            })
            .ok_or("ZIP64_END_LIMIT_OR_INVALID")?;
        if !(44..=MAX_ZIP64_END as u64 - 12).contains(&u64_at(&tail, record + 4))
            || u32_at(&tail, record + 16) != 0
            || u32_at(&tail, record + 20) != 0
            || u64_at(&tail, record + 24) != u64_at(&tail, record + 32)
            || u64_at(&tail, locator + 8) > base + record as u64
        {
            return Err("ZIP64_END_LIMIT_OR_INVALID".into());
        }
        zip64_end = Some(base + record as u64);
        (u64_at(&tail, record + 32), u64_at(&tail, record + 40))
    } else {
        if u16_at(&tail, end + 4) != 0
            || u16_at(&tail, end + 6) != 0
            || u16_at(&tail, end + 8) != u16_at(&tail, end + 10)
        {
            return Err("ZIP_MULTIDISK_UNSUPPORTED".into());
        }
        (
            u16_at(&tail, end + 10) as u64,
            u32_at(&tail, end + 12) as u64,
        )
    };
    if count >= MAX_NODES as u64 {
        return Err("ZIP_NODE_LIMIT".into());
    }
    if size > MAX_METADATA || size > length {
        return Err("ZIP_METADATA_LIMIT".into());
    }
    reader.seek(SeekFrom::Start(0)).map_err(zip_error)?;
    reader.read_bytes = 0;
    reader.metadata_ends = Some((base + end as u64, zip64_end));
    reader
        .metadata_phase
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(count as usize)
}

fn safe_name(name: &str, directory: bool) -> Result<PathBuf, String> {
    let normalized = name.replace('\\', "/");
    let normalized = if directory {
        normalized.strip_suffix('/').ok_or("ZIP_DIRECTORY_NAME")?
    } else {
        normalized.as_str()
    };
    let mut relative = PathBuf::new();
    for part in normalized.split('/') {
        if part.is_empty()
            || matches!(part, "." | "..")
            || part.ends_with(['.', ' '])
            || part.encode_utf16().count() > 255
            || part.chars().any(|ch| ch < ' ' || "<>:\"|?*".contains(ch))
        {
            return Err("ZIP_PATH_INVALID".into());
        }
        let base = part.split('.').next().unwrap().to_uppercase();
        if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&base.as_str())
            || ["COM", "LPT"].iter().any(|prefix| {
                base.strip_prefix(prefix).is_some_and(|suffix| {
                    suffix.len() == 1
                        && matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                        || matches!(suffix, "¹" | "²" | "³")
                })
            })
        {
            return Err("ZIP_DEVICE_NAME".into());
        }
        relative.push(part);
    }
    if relative.components().count() > 64 {
        return Err("ZIP_DEPTH_LIMIT".into());
    }
    Ok(relative)
}

#[derive(Clone)]
struct Entry {
    relative: PathBuf,
    directory: bool,
    readonly: bool,
    index: Option<usize>,
    size: u64,
}
fn key(path: &Path) -> Result<String, String> {
    Ok(path.to_str().ok_or("ZIP_NAME_ENCODING")?.to_lowercase())
}
fn inventory(
    c: &Core,
    archive: &mut ZipArchive<CheckedIo<'_>>,
    destination: &Path,
    control: &Control,
) -> Result<Vec<Entry>, String> {
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        control.check(c)?;
        let file = archive.by_index_raw(i).map_err(zip_error)?;
        if file.encrypted() {
            return Err("ZIP_ENCRYPTED_UNSUPPORTED".into());
        }
        if !matches!(
            file.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err("ZIP_COMPRESSION_UNSUPPORTED".into());
        }
        let directory = file.is_dir();
        let mode = file.unix_mode().unwrap_or(0);
        if file.is_symlink()
            || !matches!(mode & 0o170000, 0 | 0o100000 | 0o040000)
            || (mode & 0o170000 == 0o040000 && !directory)
            || (mode & 0o170000 == 0o100000 && directory)
        {
            return Err("ZIP_LINK_OR_SPECIAL_FILE".into());
        }
        let relative = safe_name(file.name(), directory)?;
        if file.enclosed_name().is_none() {
            return Err("ZIP_TRAVERSAL".into());
        }
        if crate::policy::protected(
            destination
                .join(&relative)
                .to_str()
                .ok_or("ZIP_NAME_ENCODING")?,
        ) {
            return Err("PATH_PROTECTED: archive entry".into());
        }
        if directory && file.size() != 0 {
            return Err("ZIP_DIRECTORY_DATA".into());
        }
        total = total.checked_add(file.size()).ok_or("ZIP_SIZE_LIMIT")?;
        if total > MAX_BYTES {
            return Err("ZIP_SIZE_LIMIT".into());
        }
        let k = key(&relative)?;
        if let Some(prior) = entries.get(&k) {
            if prior.relative != relative || prior.index.is_some() || !prior.directory || !directory
            {
                return Err("ZIP_DUPLICATE_OR_CASE_COLLISION".into());
            }
        }
        let readonly = file.unix_mode().is_some_and(|m| m & 0o222 == 0);
        entries.insert(
            k,
            Entry {
                relative: relative.clone(),
                directory,
                readonly,
                index: Some(i),
                size: file.size(),
            },
        );
        let mut parent = relative.parent();
        while let Some(path) = parent.filter(|p| !p.as_os_str().is_empty()) {
            let k = key(path)?;
            if let Some(prior) = entries.get(&k) {
                if !prior.directory || prior.relative != path {
                    return Err("ZIP_FILE_DIRECTORY_CONFLICT".into());
                }
            } else {
                entries.insert(
                    k,
                    Entry {
                        relative: path.to_path_buf(),
                        directory: true,
                        readonly: false,
                        index: None,
                        size: 0,
                    },
                );
            }
            parent = path.parent();
        }
        if entries.len() >= MAX_NODES {
            return Err("ZIP_NODE_LIMIT".into());
        }
    }
    if archive.has_overlapping_files().map_err(zip_error)? {
        return Err("ZIP_OVERLAPPING_DATA".into());
    }
    let mut result = entries.into_values().collect::<Vec<_>>();
    result.sort_by_key(|e| (e.relative.components().count(), e.relative.clone()));
    Ok(result)
}

struct Known {
    id: String,
    directory: bool,
    readonly: bool,
    digest: Option<String>,
}
fn validate_known(
    c: &Core,
    tree: &Tree,
    known: &BTreeMap<PathBuf, Known>,
    control: &Control,
) -> Result<(), String> {
    if tree.nodes.len() != known.len() {
        return Err("ZIP_STAGE_ENTRY_CHANGED".into());
    }
    for node in &tree.nodes {
        let expected = known.get(&node.relative).ok_or("ZIP_STAGE_ENTRY_CHANGED")?;
        if expected.id != identity(&node.file)?
            || expected.directory != node.directory
            || expected.readonly != node.readonly
        {
            return Err("ZIP_STAGE_OBJECT_CHANGED".into());
        }
        if let Some(digest) = &expected.digest {
            if hash_file_limit(
                &mut node.file.try_clone().map_err(zip_error)?,
                MAX_BYTES,
                || control.check(c),
            )? != *digest
            {
                return Err("ZIP_STAGE_CONTENT_CHANGED".into());
            }
        }
    }
    Ok(())
}
fn source_unchanged(c: &Core, path: &Path, source: &Tree, control: &Control) -> Result<(), String> {
    let current = Tree::capture(c, path, false, control)?;
    if current.identity != source.identity
        || current.digest != source.digest
        || current.ownership()? != source.ownership()?
    {
        return Err("ZIP_SOURCE_CHANGED".into());
    }
    Ok(())
}
fn stage(c: &Core, destination: &Path, token: &str, original: &Path) -> Result<PathBuf, String> {
    let path = staging_path(c, destination, destination.is_dir(), original)?;
    c.store.execute(
        "UPDATE undo_records SET staging=? WHERE id=?",
        &[json!(path), json!(token)],
    )?;
    Ok(path)
}
fn record_stage(c: &Core, token: &str, file: &File) -> Result<(), String> {
    let mut data = journal_metadata(c, token)?;
    data["stage_id"] = json!(identity(file)?);
    save_metadata(c, token, &data)
}
fn ready(
    c: &Core,
    token: &str,
    tree: &Tree,
    operation: &str,
    batch: Option<(&str, usize)>,
) -> Result<(), String> {
    let mut data = journal_metadata(c, token)?;
    data["operation"] = json!(operation);
    data["result_nodes"] = tree.ownership()?;
    if let Some((id, index)) = batch {
        data["archive_batch"] = json!(id);
        data["archive_index"] = json!(index);
    }
    save_metadata(c, token, &data)?;
    c.store.execute(
        "UPDATE undo_records SET object_id=?,expected_hash=? WHERE id=?",
        &[json!(tree.identity), json!(tree.digest), json!(token)],
    )?;
    Ok(())
}
fn publish(
    c: &Core,
    token: &str,
    stage: &Path,
    destination: &Path,
    tree: Tree,
    control: &Control,
) -> Result<(), String> {
    let digest = tree.digest.clone();
    let id = tree.identity.clone();
    drop(tree);
    relocate(c, stage, destination, &digest, &id, control)?;
    #[cfg(all(test, windows))]
    crash_tests::at(c, "zip_namespace_published");
    c.store.execute(
        "UPDATE undo_records SET staging=NULL WHERE id=?",
        &[json!(token)],
    )?;
    undo::commit(c, token)
}
fn failed(c: &Core, token: &str, error: String) -> String {
    // Startup can still reconcile a successfully published prepared tree_copy.
    let row = c
        .store
        .query("SELECT * FROM undo_records WHERE id=?", &[json!(token)])
        .ok()
        .map(|r| r[0].clone());
    if !row.as_ref().is_some_and(|r| {
        r["kind"] == "tree_copy" && expected_state(c, r, &Control::default()).is_ok()
    }) {
        let _ = c.store.execute(
            "UPDATE undo_records SET state='blocked',updated_at=? WHERE id=? AND state='prepared'",
            &[json!(storage::now()), json!(token)],
        );
    }
    format!("{error}; recovery_undo_id={token}")
}

pub(super) fn compress(
    c: &Core,
    path: &Path,
    destination: &Path,
    control: &Control,
) -> Result<Value, String> {
    control.check(c)?;
    let _parents = parents(c, destination)?;
    if destination.exists() {
        return Err("DESTINATION_EXISTS".into());
    }
    if destination.starts_with(path) || path.starts_with(destination) {
        return Err("TREE_OVERLAPPING_PATHS".into());
    }
    let source = Tree::capture(c, path, false, control)?;
    if source.nodes.len() >= MAX_NODES {
        return Err("ZIP_NODE_LIMIT".into());
    }
    let basename = path
        .file_name()
        .and_then(|p| p.to_str())
        .ok_or("ZIP_NAME_ENCODING")?;
    safe_name(basename, false)?;
    let used = check_quota(c, 0, control)?;
    let retained = checkpoint_path_avoiding(c, destination, Some(path))?;
    let token = undo::prepare_tree(c, "tree_copy", path, Some(destination), Some(&retained), "")?;
    let result = (|| {
        let stage = stage(c, destination, &token, path)?;
        let output = write_handle(&stage, true)?;
        record_stage(c, &token, &output)?;
        let id = identity(&output)?;
        let mut io = checked(output, c, control);
        io.output_limit = MAX_BYTES.min(MAX_CHECKPOINT_BYTES - used);
        {
            let mut writer = ZipWriter::new(&mut io);
            for node in &source.nodes {
                control.check(c)?;
                let relative = if node.relative.as_os_str().is_empty() {
                    PathBuf::from(basename)
                } else {
                    Path::new(basename).join(&node.relative)
                };
                let name = relative
                    .to_str()
                    .ok_or("ZIP_NAME_ENCODING")?
                    .replace('\\', "/");
                safe_name(&name, false)?;
                let permissions = if node.directory {
                    if node.readonly {
                        0o555
                    } else {
                        0o755
                    }
                } else if node.readonly {
                    0o444
                } else {
                    0o644
                };
                let options = SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .unix_permissions(permissions);
                if node.directory {
                    writer.add_directory(name, options).map_err(zip_error)?;
                } else {
                    writer.start_file(name, options).map_err(zip_error)?;
                    let mut input = node.file.try_clone().map_err(zip_error)?;
                    input.seek(SeekFrom::Start(0)).map_err(zip_error)?;
                    let mut buffer = [0; 65_536];
                    loop {
                        control.check(c)?;
                        let n = input.read(&mut buffer).map_err(zip_error)?;
                        if n == 0 {
                            break;
                        }
                        writer.write_all(&buffer[..n]).map_err(zip_error)?;
                    }
                }
            }
            writer.finish().map_err(zip_error)?;
        }
        io.file.sync_all().map_err(zip_error)?;
        let digest = hash_file_limit(&mut io.file, MAX_BYTES, || control.check(c))?;
        drop(io);
        source_unchanged(c, path, &source, control)?;
        let output = Tree::capture(c, &stage, false, control)?;
        let known = BTreeMap::from([(
            PathBuf::new(),
            Known {
                id,
                directory: false,
                readonly: false,
                digest: Some(digest),
            },
        )]);
        validate_known(c, &output, &known, control)?;
        check_quota(c, 0, control)?;
        ready(c, &token, &output, "zip_compress", None)?;
        let bytes = output.bytes;
        publish(c, &token, &stage, destination, output, control)?;
        Ok(
            json!({"path":destination,"entries":source.nodes.len(),"bytes":bytes,"undo_token":token,"undo_available":true}),
        )
    })();
    result.map_err(|error| failed(c, &token, error))
}

fn merge_groups(
    c: &Core,
    destination: &Path,
    entries: &[Entry],
    control: &Control,
) -> Result<(Vec<PathBuf>, Vec<File>), String> {
    let mut groups = Vec::new();
    let mut leases = vec![open(destination, false, true)?];
    for entry in entries {
        control.check(c)?;
        if groups
            .iter()
            .any(|g: &PathBuf| entry.relative.starts_with(g))
        {
            continue;
        }
        let target = destination.join(&entry.relative);
        if target.exists() {
            c.path(target.to_str().ok_or("ZIP_NAME_ENCODING")?, false)?;
            if !entry.directory || !target.is_dir() {
                return Err("ZIP_DESTINATION_EXISTS".into());
            }
            leases.push(open(&target, false, true)?);
        } else {
            groups.push(entry.relative.clone());
        }
    }
    Ok((groups, leases))
}
pub(super) fn extract(
    c: &Core,
    path: &Path,
    destination: &Path,
    control: &Control,
) -> Result<Value, String> {
    control.check(c)?;
    let _parents = parents(c, destination)?;
    if destination.starts_with(path) {
        return Err("TREE_OVERLAPPING_PATHS".into());
    }
    let source = Tree::capture(c, path, false, control)?;
    if source.nodes[0].directory {
        return Err("ZIP_SOURCE_NOT_FILE".into());
    }
    let mut input = checked(source.root.try_clone().map_err(zip_error)?, c, control);
    let count = trailer(&mut input)?;
    let phase = input.metadata_phase.clone();
    let ambiguous = input.ambiguous_end.clone();
    let built = ZipArchive::new(input);
    if ambiguous.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("ZIP_AMBIGUOUS_END".into());
    }
    let mut archive = built.map_err(zip_error)?;
    // Construction has finished; local headers and compressed payload now use
    // the streaming input budget. The validated metadata is immutable.
    phase.store(false, std::sync::atomic::Ordering::Relaxed);
    if archive.len() != count {
        return Err("ZIP_DUPLICATE_OR_INVALID_COUNT".into());
    }
    let entries = inventory(c, &mut archive, destination, control)?;
    let merge = destination.exists();
    let (groups, _leases) = if merge {
        if !destination.is_dir() {
            return Err("ZIP_DESTINATION_NOT_DIRECTORY".into());
        }
        merge_groups(c, destination, &entries, control)?
    } else {
        (vec![PathBuf::new()], Vec::new())
    };
    let declared = entries.iter().map(|e| e.size).sum();
    check_quota(c, declared, control)?;
    let retained = if merge {
        None
    } else {
        Some(checkpoint_path(c, destination)?)
    };
    let token = undo::prepare_tree(
        c,
        if merge { "archive_batch" } else { "tree_copy" },
        path,
        Some(destination),
        retained.as_deref(),
        "",
    )?;
    let result = (|| {
        let stage = stage(c, destination, &token, path)?;
        std::fs::create_dir(&stage).map_err(zip_error)?;
        let guard = open(&stage, false, true)?;
        record_stage(c, &token, &guard)?;
        let mut known = BTreeMap::from([(
            PathBuf::new(),
            Known {
                id: identity(&guard)?,
                directory: true,
                readonly: false,
                digest: None,
            },
        )]);
        for entry in &entries {
            control.check(c)?;
            let target = stage.join(&entry.relative);
            c.path(target.to_str().ok_or("ZIP_NAME_ENCODING")?, true)?;
            let (id, digest) = if entry.directory {
                std::fs::create_dir(&target).map_err(zip_error)?;
                if let Some(index) = entry.index {
                    let mut data = archive.by_index(index).map_err(zip_error)?;
                    let mut byte = [0];
                    if data.read(&mut byte).map_err(zip_error)? != 0 {
                        return Err("ZIP_DIRECTORY_DATA".into());
                    }
                }
                (identity(&open(&target, false, false)?)?, None)
            } else {
                let mut output = write_handle(&target, true)?;
                let id = identity(&output)?;
                let mut data = archive
                    .by_index(entry.index.ok_or("ZIP_ENTRY_INDEX")?)
                    .map_err(zip_error)?;
                let mut hash = Sha256::new();
                let mut written = 0u64;
                let mut buffer = [0; 65_536];
                loop {
                    control.check(c)?;
                    let n = data.read(&mut buffer).map_err(zip_error)?;
                    if n == 0 {
                        break;
                    }
                    written = written.saturating_add(n as u64);
                    if written > entry.size {
                        return Err("ZIP_DECLARED_SIZE_MISMATCH".into());
                    }
                    output.write_all(&buffer[..n]).map_err(zip_error)?;
                    hash.update(&buffer[..n]);
                }
                if written != entry.size {
                    return Err("ZIP_DECLARED_SIZE_MISMATCH".into());
                }
                output.sync_all().map_err(zip_error)?;
                (id, Some(format!("{:x}", hash.finalize())))
            };
            known.insert(
                entry.relative.clone(),
                Known {
                    id,
                    directory: entry.directory,
                    readonly: entry.readonly,
                    digest,
                },
            );
        }
        for entry in entries.iter().rev() {
            let target = stage.join(&entry.relative);
            let mut permissions = std::fs::metadata(&target).map_err(zip_error)?.permissions();
            permissions.set_readonly(entry.readonly);
            std::fs::set_permissions(target, permissions).map_err(zip_error)?;
        }
        source_unchanged(c, path, &source, control)?;
        let output = Tree::capture(c, &stage, false, control)?;
        validate_known(c, &output, &known, control)?;
        check_quota(c, 0, control)?;
        if !merge {
            ready(c, &token, &output, "zip_extract", None)?;
            drop(guard);
            publish(c, &token, &stage, destination, output, control)?;
            return Ok(
                json!({"path":destination,"bytes":declared,"entries":entries.len(),"undo_token":token,"undo_available":true}),
            );
        }
        drop(output);
        let mut ids = Vec::new();
        for (index, relative) in groups.iter().enumerate() {
            control.check(c)?;
            let staged = stage.join(relative);
            let target = destination.join(relative);
            let output = Tree::capture(c, &staged, false, control)?;
            let subset = known
                .iter()
                .filter_map(|(p, k)| {
                    p.strip_prefix(relative).ok().map(|r| {
                        (
                            r.to_path_buf(),
                            Known {
                                id: k.id.clone(),
                                directory: k.directory,
                                readonly: k.readonly,
                                digest: k.digest.clone(),
                            },
                        )
                    })
                })
                .collect();
            validate_known(c, &output, &subset, control)?;
            let retained = checkpoint_path(c, &target)?;
            let child = undo::prepare_tree(
                c,
                "tree_copy",
                path,
                Some(&target),
                Some(&retained),
                &output.digest,
            )?;
            c.store.execute(
                "UPDATE undo_records SET staging=? WHERE id=?",
                &[json!(staged), json!(child)],
            )?;
            record_stage(c, &child, &output.root)?;
            ready(c, &child, &output, "zip_extract", Some((&token, index)))?;
            // Every group is discoverable by archive_batch before its rename.
            ids.push(child.clone());
            publish(c, &child, &staged, &target, output, control)?;
        }
        // The only residuals are directories already present in the destination.
        // Their IDs must still be ours, and their emptiness is checked natively.
        let mut remaining = entries
            .iter()
            .filter(|e| e.directory)
            .map(|e| e.relative.clone())
            .collect::<Vec<_>>();
        remaining.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        for relative in remaining {
            let target = stage.join(&relative);
            if !target.exists() {
                continue;
            }
            let file = open(&target, true, false)?;
            if identity(&file)? != known[&relative].id {
                return Err("ZIP_STAGE_OBJECT_CHANGED".into());
            }
            cleanup::delete_handle(&file)?;
        }
        let root_id = identity(&guard)?;
        drop(guard);
        let root = open(&stage, true, false)?;
        if identity(&root)? != root_id {
            return Err("ZIP_STAGE_OBJECT_CHANGED".into());
        }
        cleanup::delete_handle(&root)?;
        drop(root);
        c.store.execute(
            "UPDATE undo_records SET state='completed',staging=NULL,updated_at=? WHERE id=?",
            &[json!(storage::now()), json!(token)],
        )?;
        Ok(
            json!({"path":destination,"bytes":declared,"entries":entries.len(),"undo_tokens":ids,"undo_available":!ids.is_empty(),"archive_batch_id":token,"merge":true}),
        )
    })();
    result.map_err(|error| failed(c, &token, error))
}

pub(super) fn status(c: &Core, id: &str, offset: u64) -> Result<Value, String> {
    uuid::Uuid::parse_str(id).map_err(|_| "ARCHIVE_BATCH_ID")?;
    if offset > MAX_NODES as u64 {
        return Err("ARCHIVE_OFFSET_LIMIT".into());
    }
    let parent = c.store.query(
        "SELECT id,kind,path,destination,staging,state FROM undo_records WHERE id=?",
        &[json!(id)],
    )?[0]
        .clone();
    if parent["kind"] != "archive_batch" {
        return Err("ARCHIVE_BATCH_NOT_FOUND".into());
    }
    let children=c.store.query("SELECT id,destination,checkpoint,staging,state FROM undo_records WHERE json_extract(metadata_json,'$.archive_batch')=? ORDER BY CAST(json_extract(metadata_json,'$.archive_index') AS INTEGER) LIMIT 100 OFFSET ?",&[json!(id),json!(offset)])?;
    let count=c.store.query("SELECT count(*) AS n FROM undo_records WHERE json_extract(metadata_json,'$.archive_batch')=?",&[json!(id)])?[0]["n"].clone();
    let returned = children.as_array().ok_or("ARCHIVE_ROWS")?.len();
    Ok(
        json!({"batch":parent,"operations":children,"count":count,"offset":offset,"next_offset":if offset+(returned as u64)<count.as_u64().unwrap_or(0){json!(offset+returned as u64)}else{Value::Null}}),
    )
}

#[cfg(all(test, windows))]
#[path = "file_zip_tests.rs"]
mod tests;
