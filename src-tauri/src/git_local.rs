//! Offline, observed Git operations. No hooks, filters, credential helpers,
//! signing programs, network protocols or arbitrary revisions are executed.
use crate::{asset_files, policy, process, runtime::Core};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub(crate) const TOOLS: &[&str] = &[
    "git.status",
    "git.diff",
    "git.branches",
    "git.log",
    "git.commits",
    "git.commit",
    "git.checkout",
    "git.stash",
    "git.delete_branch",
];
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or_else(|| format!("GIT_INPUT_{key}"))
}
fn text(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|_| "GIT_OUTPUT_UTF8".into())
}
fn oid(value: &str) -> bool {
    [40, 64].contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn branch(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && !value.starts_with(['-', '.', '/'])
        && !value.ends_with(['.', '/'])
        && !value.ends_with(".lock")
        && !["..", "@{", "//"].iter().any(|s| value.contains(s))
        && value != "@"
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_/.-".contains(c))
        && value
            .split('/')
            .all(|part| !part.starts_with('.') && !part.ends_with(".lock"))
}
pub(crate) fn schema(tool: &str) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "path".into(),
        json!({"type":"string","minLength":1,"maxLength":4000}),
    );
    let mut required = vec!["path"];
    match tool {
        "git.commit" | "git.checkout" | "git.stash" | "git.delete_branch" => {
            properties.insert("expected".into(),json!({"type":"object","required":["fingerprint"],"properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"additionalProperties":false}));
            required.push("expected");
            if tool == "git.commit" {
                properties.insert(
                    "message".into(),
                    json!({"type":"string","minLength":1,"maxLength":4000}),
                );
                required.push("message");
            }
            if ["git.checkout", "git.delete_branch"].contains(&tool) {
                properties.insert("branch".into(),json!({"type":"string","minLength":1,"maxLength":200,"pattern":"^[a-zA-Z0-9_][a-zA-Z0-9_./-]*$"}));
                required.push("branch");
            }
        }
        "git.diff" => {
            properties.insert("staged".into(), json!({"type":"boolean"}));
        }
        "git.log" | "git.commits" => {
            properties.insert(
                "limit".into(),
                json!({"type":"integer","minimum":1,"maximum":100}),
            );
            properties.insert(
                "offset".into(),
                json!({"type":"integer","minimum":0,"maximum":10000}),
            );
        }
        "git.status" | "git.branches" => (),
        _ => return json!({"not":{}}),
    }
    json!({"type":"object","required":required,"properties":properties,"additionalProperties":false})
}

struct Repo {
    path: PathBuf,
    git_dir: PathBuf,
    common: PathBuf,
    exe: PathBuf,
    identities: Value,
    _leases: Vec<File>,
    isolated: bool,
    user_name: String,
    user_email: String,
    signing_required: bool,
    safe_config: Vec<(&'static str, String)>,
}
struct Scope {
    core: Arc<Core>,
    roots: Vec<PathBuf>,
}
impl Scope {
    fn check(&self) -> Result<(), String> {
        if !self.core.enabled("git") {
            return Err("MODULE_DISABLED: git".into());
        }
        if self.core.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if self.core.roots() != self.roots {
            return Err("GIT_SCOPE_CHANGED".into());
        }
        Ok(())
    }
}
impl Repo {
    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.env_clear();
        for key in [
            "SystemRoot",
            "WINDIR",
            "PATH",
            "USERPROFILE",
            "HOMEDRIVE",
            "HOMEPATH",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
        ] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        if let Some(root) = std::env::var_os("SystemRoot") {
            cmd.env("COMSPEC", PathBuf::from(root).join("System32/cmd.exe"));
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_ATTR_NOSYSTEM", "1");
        if self.isolated {
            cmd.env("GIT_CONFIG_GLOBAL", "NUL")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_DIR", git_path(&self.git_dir))
                .env("GIT_COMMON_DIR", git_path(&self.common))
                .env("GIT_WORK_TREE", git_path(&self.path));
            if !self.user_name.is_empty() {
                cmd.arg("-c").arg(format!("user.name={}", self.user_name));
            }
            if !self.user_email.is_empty() {
                cmd.arg("-c").arg(format!("user.email={}", self.user_email));
            }
            for (key, value) in &self.safe_config {
                cmd.arg("-c").arg(format!("{key}={value}"));
            }
        }
        cmd.current_dir(&self.path)
            .arg("--no-pager")
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
                "-c",
                "protocol.allow=never",
                "-c",
                "submodule.recurse=false",
                "-c",
                "core.attributesFile=NUL",
                "-c",
                "color.ui=false",
            ])
            .args(["-c", "core.hooksPath=NUL"])
            .arg("-C")
            .arg(git_path(&self.path));
        cmd
    }
    async fn run(
        &self,
        scope: &Scope,
        args: &[&str],
        stdin: Option<&[u8]>,
        nonzero: bool,
    ) -> Result<Vec<u8>, String> {
        let mut cmd = self.command();
        cmd.args(args);
        let missing_code = if args.first() == Some(&"rev-parse") {
            128
        } else {
            1
        };
        run_command(
            scope,
            cmd,
            stdin,
            if nonzero { Some(missing_code) } else { None },
        )
        .await
    }

    async fn value(&self, scope: &Scope, args: &[&str], nonzero: bool) -> Result<String, String> {
        Ok(text(&self.run(scope, args, None, nonzero).await?)?
            .trim()
            .to_owned())
    }
    async fn config(&self, scope: &Scope) -> Result<String, String> {
        // Read names, never configuration values (which may contain credentials).
        let names=self.value(scope,&["config","--local","--no-includes","--name-only","--get-regexp","^(include\\.|includeif\\.|extensions\\.(partialclone|worktreeconfig)|remote\\..*\\.promisor|core\\.worktree)"],true).await?;
        if !names.is_empty() {
            return Err("GIT_PARTIAL_OR_REDIRECTED_REPOSITORY_DENIED".into());
        }
        let names = self
            .value(
                scope,
                &[
                    "config",
                    "--name-only",
                    "--get-regexp",
                    "^filter\\..*\\.(clean|smudge|process|required)$",
                ],
                true,
            )
            .await?;
        if !names.is_empty() {
            return Err("GIT_FILTER_EXECUTION_DENIED".into());
        }
        // This digest binds effective configuration, without exposing it in reviews.
        let data = self
            .run(scope, &["config", "--null", "--list"], None, false)
            .await?;
        Ok(hash(&data))
    }
}

fn executable() -> Result<PathBuf, String> {
    let search = std::env::var_os("PATH").ok_or("GIT_NOT_INSTALLED")?;
    for dir in std::env::split_paths(&search).filter(|d| d.is_absolute()) {
        let path = dir.join(if cfg!(windows) { "git.exe" } else { "git" });
        if path.is_file() {
            return path
                .canonicalize()
                .map_err(|_| "GIT_EXECUTABLE_PATH".into());
        }
    }
    Err("GIT_NOT_INSTALLED".into())
}
fn lease(path: &Path, dirs: &mut Vec<File>) -> Result<String, String> {
    let file = asset_files::directory(path).map_err(|_| "GIT_DIRECTORY_IDENTITY")?;
    let identity = asset_files::identity(&file).map_err(|_| "GIT_DIRECTORY_IDENTITY")?;
    dirs.push(file);
    Ok(identity)
}
#[cfg(windows)]
pub(crate) fn executable_lease(path: &Path) -> Result<(File, String), String> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
        },
    };
    // Git for Windows legitimately hardlinks git.exe and git-lfs.exe. A held
    // no-write/no-delete file handle protects the underlying object and aliases.
    let file = std::fs::OpenOptions::new()
        .access_mode(0x80000080)
        .share_mode(1)
        .custom_flags(0x00200000)
        .open(path)
        .map_err(|_| "GIT_EXECUTABLE_LEASE")?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(|_| "GIT_EXECUTABLE_IDENTITY")?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        || !file
            .metadata()
            .map_err(|_| "GIT_EXECUTABLE_METADATA")?
            .is_file()
    {
        return Err("GIT_EXECUTABLE_IDENTITY".into());
    }
    let identity = format!(
        "{:08x}:{:08x}{:08x}",
        info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
    );
    Ok((file, identity))
}
#[cfg(not(windows))]
pub(crate) fn executable_lease(_: &Path) -> Result<(File, String), String> {
    Err("WINDOWS_11_REQUIRED".into())
}
async fn repository(scope: &Scope, path: &str) -> Result<Repo, String> {
    scope.check()?;
    let path = scope.core.path(path, false)?;
    if !path.is_dir() {
        return Err("GIT_DIRECTORY_REQUIRED".into());
    }
    let mut leases = Vec::new();
    let root_id = lease(&path, &mut leases)?;
    for parent in path.ancestors().skip(1) {
        lease(parent, &mut leases)?;
    }
    let exe = executable()?;
    let (exe_file, exe_id) = executable_lease(&exe)?;
    leases.push(exe_file);
    // Validate a worktree pointer before letting Git discover its configuration.
    let marker = path.join(".git");
    if marker.is_file() {
        let bytes = crate::modules::bounded_read(&marker)?;
        if bytes.len() > 4096 {
            return Err("GIT_WORKTREE_POINTER".into());
        }
        let target = text(&bytes)?
            .trim()
            .strip_prefix("gitdir: ")
            .ok_or("GIT_WORKTREE_POINTER")?;
        scope.core.path(
            path.join(target).to_str().ok_or("GIT_PATH_ENCODING")?,
            false,
        )?;
        leases.push(asset_files::open(&marker, false).map_err(|_| "GIT_WORKTREE_POINTER")?);
    } else if !marker.is_dir() {
        return Err("GIT_WORKTREE_REQUIRED".into());
    }
    let mut repo = Repo {
        path,
        git_dir: PathBuf::new(),
        common: PathBuf::new(),
        exe,
        identities: json!([root_id, exe_id]),
        _leases: leases,
        isolated: false,
        user_name: String::new(),
        user_email: String::new(),
        signing_required: false,
        safe_config: Vec::new(),
    };
    // Explicitly deny parent-repository discovery and bare repositories.
    let top = repo
        .value(scope, &["rev-parse", "--show-toplevel"], false)
        .await?;
    if Path::new(&top).canonicalize().map_err(|_| "GIT_TOPLEVEL")? != repo.path {
        return Err("GIT_EXACT_ROOT_REQUIRED".into());
    }
    let git_dir = repo
        .value(scope, &["rev-parse", "--absolute-git-dir"], false)
        .await?;
    let common = repo
        .value(
            scope,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            false,
        )
        .await?;
    repo.git_dir = scope.core.path(&git_dir, false)?;
    repo.common = scope.core.path(&common, false)?;
    let git_id = lease(&repo.git_dir, &mut repo._leases)?;
    let common_id = lease(&repo.common, &mut repo._leases)?;
    repo.identities = json!([repo.identities, git_id, common_id]);
    // Worktree metadata and common object store must both be in approved roots.
    for p in [&repo.git_dir, &repo.common] {
        for parent in p.ancestors().skip(1) {
            lease(parent, &mut repo._leases)?;
        }
    }
    for name in ["config", "config.worktree", "commondir"] {
        for dir in [&repo.git_dir, &repo.common] {
            let file = dir.join(name);
            if file.exists() {
                repo._leases
                    .push(asset_files::open(&file, false).map_err(|_| "GIT_CONFIG_LEASE")?);
            }
        }
    }
    repo.isolated = true;
    repo.config(scope).await?;
    repo.isolated = false;
    // Only safe identity fields are imported from ambient configuration. All
    // subsequent operations ignore system/global config and reject local includes.
    repo.user_name = repo
        .value(scope, &["config", "--get", "user.name"], true)
        .await?;
    repo.user_email = repo
        .value(scope, &["config", "--get", "user.email"], true)
        .await?;
    let signing = repo
        .value(
            scope,
            &["config", "--type=bool", "--get", "commit.gpgsign"],
            true,
        )
        .await?;
    repo.signing_required =
        ["true", "yes", "on", "1"].contains(&signing.to_ascii_lowercase().as_str());
    for key in [
        "core.autocrlf",
        "core.eol",
        "core.filemode",
        "core.ignorecase",
        "core.longpaths",
    ] {
        let value = if ["core.autocrlf", "core.eol"].contains(&key) {
            let pattern = format!("^{}$", key.replace('.', "\\."));
            let values = repo
                .value(scope, &["config", "--get-regexp", &pattern], true)
                .await?;
            let Some(line) = values.lines().last() else {
                continue;
            };
            if line == key && key == "core.autocrlf" {
                "true".to_string()
            } else {
                line.strip_prefix(&format!("{key} "))
                    .ok_or("GIT_SAFE_CONFIG_VALUE_UNSUPPORTED")?
                    .to_string()
            }
        } else {
            repo.value(scope, &["config", "--type=bool", "--get", key], true)
                .await?
        };
        if !value.is_empty() {
            repo.safe_config
                .push((key, safe_config_value(key, &value)?));
        }
    }
    for identity in [&repo.user_name, &repo.user_email] {
        if identity.len() > 500
            || identity.chars().any(char::is_control)
            || policy::contains_secret(identity)
        {
            return Err("GIT_IDENTITY_DENIED".into());
        }
    }
    repo.isolated = true;
    repo.config(scope).await?;
    Ok(repo)
}

struct Snapshot {
    fingerprint: String,
    head: String,
    branch: String,
    status: String,
    staged: Vec<u8>,
    working: Vec<u8>,
    branches: Vec<(String, String)>,
    staged_tree: String,
}
impl Snapshot {
    fn data(&self) -> Value {
        json!({"expected":{"fingerprint":self.fingerprint},"head":if self.head.is_empty(){Value::Null}else{json!(self.head)},"branch":if self.branch.is_empty(){Value::Null}else{json!(self.branch)},"status":self.status.split('\0').filter(|s|!s.is_empty()).collect::<Vec<_>>(),"has_staged_changes":!self.staged.is_empty(),"has_working_changes":!self.working.is_empty(),"untrusted_content":true})
    }
}
async fn snapshot(repo: &Repo, scope: &Scope) -> Result<Snapshot, String> {
    repo.config(scope).await?;
    repository_paths(repo, scope).await?;
    let head = repo
        .value(scope, &["rev-parse", "--verify", "HEAD"], true)
        .await?;
    if !head.is_empty() && !oid(&head) {
        return Err("GIT_HEAD_INVALID".into());
    }
    let branch = repo
        .value(scope, &["symbolic-ref", "--quiet", "--short", "HEAD"], true)
        .await?;
    if !branch.is_empty() && !self::branch(&branch) {
        return Err("GIT_BRANCH_UNSUPPORTED".into());
    }
    let status = repo
        .run(
            scope,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=all",
            ],
            None,
            false,
        )
        .await?;
    let working = repo
        .run(
            scope,
            &[
                "diff",
                "--binary",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--ignore-submodules=all",
            ],
            None,
            false,
        )
        .await?;
    let staged = repo
        .run(
            scope,
            &[
                "diff",
                "--cached",
                "--binary",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--ignore-submodules=all",
            ],
            None,
            false,
        )
        .await?;
    let refs = repo
        .value(
            scope,
            &[
                "for-each-ref",
                "--format=%(refname:short) %(objectname)",
                "refs/heads/",
            ],
            false,
        )
        .await?;
    let branches = refs
        .lines()
        .map(|line| {
            let (name, id) = line.rsplit_once(' ').ok_or("GIT_BRANCH_OUTPUT")?;
            if !self::branch(name) || !oid(id) {
                return Err("GIT_BRANCH_UNSUPPORTED".to_string());
            }
            Ok((name.to_owned(), id.to_owned()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if branches.len() > 512 {
        return Err("GIT_BRANCH_LIMIT".into());
    }
    let config = repo.config(scope).await?;
    // write-tree materializes the already staged tree only. It never stages files.
    // An unmerged index fails here, before permission or mutations.
    let staged_tree = repo.value(scope, &["write-tree"], false).await?;
    if !oid(&staged_tree) {
        return Err("GIT_TREE_INVALID".into());
    }
    let index = crate::modules::bounded_read(&repo.git_dir.join("index"));
    let index = match index {
        Ok(v) => hash(&v),
        Err(_) if !repo.git_dir.join("index").exists() => String::new(),
        Err(_) => return Err("GIT_INDEX_READ".into()),
    };
    let stash = repo
        .value(scope, &["rev-parse", "--verify", "refs/stash"], true)
        .await?;
    let fingerprint = hash(
        json!([
            repo.path,
            repo.identities,
            head,
            branch,
            hash(&status),
            hash(&working),
            hash(&staged),
            branches,
            config,
            staged_tree,
            index,
            stash,
            scope.roots,
            repo.signing_required
        ])
        .to_string()
        .as_bytes(),
    );
    if repo
        .value(scope, &["rev-parse", "--verify", "HEAD"], true)
        .await?
        != head
        || repo
            .value(scope, &["symbolic-ref", "--quiet", "--short", "HEAD"], true)
            .await?
            != branch
    {
        return Err("GIT_CHANGED_RETRY".into());
    }
    Ok(Snapshot {
        fingerprint,
        head,
        branch,
        status: text(&status)?.to_owned(),
        staged,
        working,
        branches,
        staged_tree,
    })
}
async fn repository_paths(repo: &Repo, scope: &Scope) -> Result<(), String> {
    for name in [
        "objects/info/alternates",
        "objects/info/http-alternates",
        "info/grafts",
    ] {
        if repo.common.join(name).exists() {
            return Err("GIT_SHARED_OR_GRAFTED_OBJECT_STORE_DENIED".into());
        }
    }
    // Metadata may contain neither reparses nor hardlinked content from another
    // namespace. Bound the walk; do not read object/config/log plaintext here.
    let mut pending = vec![repo.common.clone(), repo.git_dir.clone()];
    let mut seen = std::collections::HashSet::new();
    let mut count = 0;
    while let Some(path) = pending.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        count += 1;
        if count > 100000 {
            return Err("GIT_METADATA_LIMIT".into());
        }
        if count % 64 == 0 {
            scope.check()?;
        }
        scope
            .core
            .path(path.to_str().ok_or("GIT_PATH_ENCODING")?, false)
            .map_err(|_| "GIT_METADATA_SCOPE_DENIED")?;
        if path.is_dir() {
            let pin = asset_files::directory(&path).map_err(|_| "GIT_METADATA_LINK_DENIED")?;
            for entry in std::fs::read_dir(&path).map_err(|_| "GIT_METADATA_READ")? {
                pending.push(entry.map_err(|_| "GIT_METADATA_READ")?.path());
            }
            drop(pin);
        } else {
            drop(asset_files::open(&path, false).map_err(|_| "GIT_METADATA_LINK_DENIED")?);
        }
    }
    let paths = repo
        .run(
            scope,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
            ],
            None,
            false,
        )
        .await?;
    let paths = text(&paths)?
        .split('\0')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if paths.len() > 20000 {
        return Err("GIT_FILE_LIMIT".into());
    }
    let attribute_paths = paths.clone();
    for (index, relative) in paths.into_iter().enumerate() {
        if index % 64 == 0 {
            scope.check()?;
        }
        let relative = Path::new(relative);
        if relative.is_absolute()
            || relative
                .components()
                .any(|p| !matches!(p, std::path::Component::Normal(_)))
        {
            return Err("GIT_FILE_PATH_DENIED".into());
        }
        let path = repo.path.join(relative);
        let raw = path.to_str().ok_or("GIT_PATH_ENCODING")?;
        if policy::protected(raw) {
            return Err("GIT_FILE_SCOPE_DENIED".into());
        }
        let mut ancestor = path.as_path();
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or("GIT_FILE_SCOPE_DENIED")?;
        }
        scope
            .core
            .path(ancestor.to_str().ok_or("GIT_PATH_ENCODING")?, false)
            .map_err(|_| "GIT_FILE_SCOPE_DENIED")?;
        if path.is_file() {
            drop(asset_files::open(&path, false).map_err(|_| "GIT_FILE_LINK_OR_ACCESS_DENIED")?);
        }
    }
    // Attribute-declared filters must also fail closed when their configuration
    // lived only in the ignored global file (for example a global LFS setup).
    let attrs = repo
        .run(
            scope,
            &["check-attr", "-z", "--stdin", "filter"],
            Some(&paths_bytes(&attribute_paths)),
            false,
        )
        .await?;
    let fields = text(&attrs)?.split('\0').collect::<Vec<_>>();
    for row in fields.chunks(3) {
        if row.len() == 1 && row[0].is_empty() {
            break;
        }
        if row.len() != 3 || row[1] != "filter" {
            return Err("GIT_ATTRIBUTE_OUTPUT".into());
        }
        if !["unspecified", "unset"].contains(&row[2]) {
            return Err("GIT_FILTER_EXECUTION_DENIED".into());
        }
    }
    scope.check()
}
fn paths_bytes(paths: &[&str]) -> Vec<u8> {
    paths
        .iter()
        .flat_map(|p| p.as_bytes().iter().copied().chain(Some(0)))
        .collect()
}
fn operation_guard(repo: &Repo) -> Result<(), String> {
    for name in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
        "rebase-apply",
        "rebase-merge",
        "sequencer",
        "BISECT_LOG",
        "index.lock",
        "HEAD.lock",
        "packed-refs.lock",
    ] {
        if repo.git_dir.join(name).exists() || repo.common.join(name).exists() {
            return Err("GIT_OPERATION_IN_PROGRESS".into());
        }
    }
    Ok(())
}
pub(crate) struct Prepared {
    scope: Scope,
    repo: Repo,
    snapshot: Snapshot,
    tool: String,
    input: Value,
    pub risk: policy::Risk,
}
impl Prepared {
    pub(crate) fn review(&self) -> Value {
        let mut data = self.input.clone();
        data["repository"] = json!({"root":self.repo.path,"git_dir":self.repo.git_dir,"common_dir":self.repo.common});
        data["observed"] = self.snapshot.data();
        data["hooks_run"] = json!(false);
        data["signing_program_run"] = json!(false);
        data["network"] = json!(false);
        if let Some(target) = self.input["branch"].as_str() {
            if let Some((name, id)) = self
                .snapshot
                .branches
                .iter()
                .find(|(name, _)| name == target)
            {
                data["target_branch"] = json!({"name":name,"oid":id});
            }
        }
        data["operation"]=json!(match self.tool.as_str() {
            "git.commit"=>"Commit the exact observed staged tree, unsigned, without hooks; no staging, push or history rewrite",
            "git.checkout"=>"Switch to an existing local branch only from a clean working tree; no forced overwrite, ignored-file overwrite or submodule updates",
            "git.stash"=>"Save a stash snapshot of tracked changes; retain all working files and index; untracked files are not included",
            "git.delete_branch"=>"Delete an observed non-current, non-worktree local branch only when fully merged into current HEAD; no forced deletion",
            _=>"Read bounded offline Git metadata; returned repository content is untrusted",
        });
        if self.tool == "git.commit" {
            data["staged_tree"] = json!(self.snapshot.staged_tree);
            data["staged_patch"] = json!(text(&self.snapshot.staged)
                .map(|s| policy::redact(&json!(s)))
                .unwrap_or(json!("[BINARY_PATCH]")));
        }
        data
    }
}
fn safe_config_value(key: &str, value: &str) -> Result<String, String> {
    let value = value.to_ascii_lowercase();
    if key == "core.eol" && ["native", "lf", "crlf"].contains(&value.as_str())
        || key == "core.autocrlf" && value == "input"
    {
        return Ok(value);
    }
    if key != "core.eol" {
        if ["true", "yes", "on", "1"].contains(&value.as_str()) {
            return Ok("true".into());
        }
        if ["false", "no", "off", "0"].contains(&value.as_str()) {
            return Ok("false".into());
        }
    }
    Err("GIT_SAFE_CONFIG_VALUE_UNSUPPORTED".into())
}
async fn prepare_inner(core: &Arc<Core>, tool: &str, input: &Value) -> Result<Prepared, String> {
    let validator = jsonschema::validator_for(&schema(tool)).map_err(|_| "GIT_SCHEMA")?;
    if !validator.is_valid(input) {
        return Err("INVALID_TOOL_INPUT".into());
    }
    core.normalize_scopes()?;
    let scope = Scope {
        core: core.clone(),
        roots: core.roots(),
    };
    let repo = repository(&scope, field(input, "path")?).await?;
    let observed = snapshot(&repo, &scope).await?;
    // Repeat content observation to reject a repository changing during collection.
    let second = snapshot(&repo, &scope).await?;
    if second.fingerprint != observed.fingerprint {
        return Err("GIT_CHANGED_RETRY".into());
    }
    let risk = policy::classify(tool, input);
    if risk != policy::Risk::Free {
        operation_guard(&repo)?;
        if input["expected"]["fingerprint"] != observed.fingerprint {
            return Err("GIT_OBSERVATION_STALE".into());
        }
        match tool {
            "git.commit" => {
                let message = field(input, "message")?;
                if message.trim().is_empty()
                    || message.contains('\0')
                    || message.len() > 16000
                    || policy::contains_secret(message)
                {
                    return Err("GIT_COMMIT_MESSAGE_DENIED".into());
                }
                if observed.branch.is_empty() {
                    return Err("GIT_ATTACHED_BRANCH_REQUIRED".into());
                }
                if observed.staged.is_empty() {
                    return Err("GIT_STAGED_CHANGES_REQUIRED".into());
                }
                if text(&observed.staged).is_ok_and(policy::contains_secret) {
                    return Err("GIT_SENSITIVE_CONTENT_DENIED".into());
                }
                // Do not silently circumvent an explicit signing requirement.
                if repo.signing_required {
                    return Err("GIT_SIGNED_COMMIT_REQUIRED_UNSUPPORTED".into());
                }
                if repo.user_name.is_empty() || repo.user_email.is_empty() {
                    return Err("GIT_COMMIT_IDENTITY_REQUIRED".into());
                }
            }
            "git.checkout" => {
                if !observed.status.is_empty() {
                    return Err("GIT_CLEAN_WORKTREE_REQUIRED".into());
                }
                let target = field(input, "branch")?;
                if !branch(target) || !observed.branches.iter().any(|(name, _)| name == target) {
                    return Err("GIT_EXISTING_BRANCH_REQUIRED".into());
                }
            }
            "git.delete_branch" => {
                let target = field(input, "branch")?;
                if !branch(target)
                    || target == observed.branch
                    || !observed.branches.iter().any(|(name, _)| name == target)
                {
                    return Err("GIT_BRANCH_DELETE_DENIED".into());
                }
                if observed.head.is_empty() {
                    return Err("GIT_HEAD_REQUIRED".into());
                }
            }
            "git.stash" => {
                if observed.staged.is_empty() && observed.working.is_empty() {
                    return Err("GIT_TRACKED_CHANGES_REQUIRED".into());
                }
            }
            _ => return Err("GIT_MUTATION_UNKNOWN".into()),
        }
    }
    Ok(Prepared {
        scope,
        repo,
        snapshot: observed,
        tool: tool.into(),
        input: input.clone(),
        risk,
    })
}

pub(crate) async fn execute(prepared: Prepared) -> Result<Value, String> {
    let Prepared {
        scope,
        repo,
        snapshot: observed,
        tool,
        input,
        risk,
    } = prepared;
    let _serial = scope.core.git_actions.lock().await;
    scope.check()?;
    let now = snapshot(&repo, &scope).await?;
    if now.fingerprint != observed.fingerprint {
        return Err("GIT_OBSERVATION_STALE".into());
    }
    if risk != policy::Risk::Free {
        operation_guard(&repo)?;
    }
    let mut result = match tool.as_str() {
        "git.status" => observed.data(),
        "git.branches" => {
            json!({"branches":observed.branches.iter().map(|(name,id)|json!({"name":name,"oid":id,"current":name==&observed.branch})).collect::<Vec<_>>(),"expected":{"fingerprint":observed.fingerprint}})
        }
        "git.diff" => {
            let bytes = if input["staged"] == true {
                &observed.staged
            } else {
                &observed.working
            };
            let patch = text(bytes)?;
            if policy::contains_secret(patch) {
                return Err("GIT_SENSITIVE_CONTENT_DENIED".into());
            }
            json!({"patch":patch,"staged":input["staged"]==true,"expected":{"fingerprint":observed.fingerprint}})
        }
        "git.log" | "git.commits" => {
            if observed.head.is_empty() {
                json!({"commits":[],"next_offset":null})
            } else {
                let limit = input["limit"].as_u64().unwrap_or(20);
                let offset = input["offset"].as_u64().unwrap_or(0);
                let limit_arg = format!("--max-count={}", limit + 1);
                let offset_arg = format!("--skip={offset}");
                let data = repo
                    .run(
                        &scope,
                        &[
                            "log",
                            "--no-show-signature",
                            "--format=%H%x00%P%x00%aI%x00%s%x00",
                            &limit_arg,
                            &offset_arg,
                            "HEAD",
                            "--",
                        ],
                        None,
                        false,
                    )
                    .await?;
                let parts = text(&data)?.split('\0').collect::<Vec<_>>();
                let mut commits = Vec::new();
                for row in parts.chunks(4) {
                    if row.len() != 4 {
                        if row.iter().any(|s| !s.trim().is_empty()) {
                            return Err("GIT_LOG_OUTPUT".into());
                        }
                        break;
                    }
                    let id = row[0].trim();
                    if !oid(id) {
                        return Err("GIT_LOG_OUTPUT".into());
                    }
                    commits.push(json!({"oid":id,"parents":row[1].split_whitespace().collect::<Vec<_>>(),"authored_at":row[2],"subject":policy::redact(&json!(row[3]))}));
                }
                let more = commits.len() > limit as usize;
                commits.truncate(limit as usize);
                json!({"commits":commits,"offset":offset,"next_offset":if more{json!(offset+limit)}else{Value::Null}})
            }
        }
        "git.commit" => {
            let mut args = vec![
                "commit-tree",
                "--no-gpg-sign",
                observed.staged_tree.as_str(),
            ];
            if !observed.head.is_empty() {
                args.extend(["-p", observed.head.as_str()]);
            }
            let message = format!("{}\n", field(&input, "message")?);
            let new = text(
                &repo
                    .run(&scope, &args, Some(message.as_bytes()), false)
                    .await?,
            )?
            .trim()
            .to_owned();
            if !oid(&new) {
                return Err("GIT_COMMIT_OUTPUT".into());
            }
            // Content is the approved tree even if another actor stages new content.
            // Ref CAS rejects a concurrent commit. An abandoned object is harmless.
            let current = snapshot(&repo, &scope).await?;
            if current.fingerprint != observed.fingerprint {
                return Err("GIT_OBSERVATION_STALE_OBJECT_UNREFERENCED".into());
            }
            let target = format!("refs/heads/{}", observed.branch);
            let old = if observed.head.is_empty() {
                "0".repeat(new.len())
            } else {
                observed.head.clone()
            };
            repo.run(
                &scope,
                &[
                    "update-ref",
                    "--no-deref",
                    "-m",
                    "AlfredPet reviewed staged commit",
                    &target,
                    &new,
                    &old,
                ],
                None,
                false,
            )
            .await?;
            json!({"committed_oid":new,"tree":observed.staged_tree,"branch":observed.branch,"hooks_run":false,"signed":false,"pushed":false})
        }
        "git.checkout" => {
            repo.run(
                &scope,
                &[
                    "switch",
                    "--no-guess",
                    "--no-recurse-submodules",
                    "--no-overwrite-ignore",
                    field(&input, "branch")?,
                ],
                None,
                false,
            )
            .await?;
            json!({"switched_to":input["branch"],"forced":false})
        }
        "git.stash" => {
            let saved = repo
                .value(
                    &scope,
                    &["stash", "create", "AlfredPet reviewed tracked snapshot"],
                    false,
                )
                .await?;
            if !oid(&saved) {
                return Err("GIT_STASH_OUTPUT".into());
            }
            if snapshot(&repo, &scope).await?.fingerprint != observed.fingerprint {
                return Err("GIT_OBSERVATION_STALE_OBJECT_UNREFERENCED".into());
            }
            repo.run(
                &scope,
                &[
                    "stash",
                    "store",
                    "-m",
                    "AlfredPet reviewed tracked snapshot",
                    &saved,
                ],
                None,
                false,
            )
            .await?;
            json!({"stash_oid":saved,"working_tree_retained":true,"index_retained":true,"includes_untracked":false})
        }
        "git.delete_branch" => {
            let target = field(&input, "branch")?;
            let old = observed
                .branches
                .iter()
                .find(|(name, _)| name == target)
                .ok_or("GIT_BRANCH_DELETE_DENIED")?
                .1
                .as_str();
            let worktrees = repo
                .run(
                    &scope,
                    &["worktree", "list", "--porcelain", "-z"],
                    None,
                    false,
                )
                .await?;
            let target_ref = format!("refs/heads/{target}");
            if text(&worktrees)?
                .split('\0')
                .any(|s| s.strip_prefix("branch ") == Some(&target_ref))
            {
                return Err("GIT_BRANCH_IN_WORKTREE".into());
            }
            repo.run(
                &scope,
                &["merge-base", "--is-ancestor", old, &observed.head],
                None,
                false,
            )
            .await
            .map_err(|_| "GIT_BRANCH_NOT_MERGED")?;
            repo.run(
                &scope,
                &["update-ref", "--no-deref", "-d", &target_ref, old],
                None,
                false,
            )
            .await?;
            json!({"deleted_branch":input["branch"],"forced":false,"branch_config_retained":true})
        }
        _ => return Err("GIT_TOOL_UNKNOWN".into()),
    };
    if risk != policy::Risk::Free {
        result["after"] = snapshot(&repo, &scope).await?.data();
    }
    result["untrusted_content"] = json!(true);
    Ok(result)
}

#[cfg(all(test, windows))]
#[path = "git_local_tests.rs"]
mod tests;

// Canonical paths remain pinned internally. Git for Windows rejects verbatim
// prefixes in GIT_DIR even though -C accepts them; normalize only CLI transport.
fn git_path(path: &Path) -> std::ffi::OsString {
    let raw = path.as_os_str().to_string_lossy();
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}").into();
    }
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).into()
}
pub(crate) async fn prepare(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
) -> Result<Prepared, String> {
    tokio::time::timeout(Duration::from_secs(60), prepare_inner(core, tool, input))
        .await
        .map_err(|_| "GIT_PREPARE_TIMEOUT".to_string())?
}

async fn run_command(
    scope: &Scope,
    mut cmd: Command,
    stdin: Option<&[u8]>,
    missing_code: Option<i32>,
) -> Result<Vec<u8>, String> {
    scope.check()?;
    process::sanitize(&mut cmd);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|_| "GIT_PROCESS_NOT_FOUND")?;
    #[cfg(windows)]
    let _job = crate::windows_native::ProcessJob::attach(&child)?;
    let mut input = child.stdin.take().ok_or("GIT_STDIN")?;
    let stdout = child.stdout.take().ok_or("GIT_STDOUT")?;
    let stderr = child.stderr.take().ok_or("GIT_STDERR")?;
    async fn bounded<R: tokio::io::AsyncRead + Unpin>(mut stream: R) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = stream
                .read(&mut buf)
                .await
                .map_err(|_| "GIT_PROCESS_READ")?;
            if n == 0 {
                return Ok(bytes);
            }
            if bytes.len() + n > OUTPUT_LIMIT {
                return Err("GIT_OUTPUT_LIMIT".into());
            }
            bytes.extend_from_slice(&buf[..n]);
        }
    }
    let operation = async {
        tokio::try_join!(
            async {
                if let Some(bytes) = stdin {
                    input
                        .write_all(bytes)
                        .await
                        .map_err(|_| "GIT_STDIN".to_string())?;
                }
                drop(input);
                Ok::<_, String>(())
            },
            async {
                child
                    .wait()
                    .await
                    .map_err(|_| "GIT_PROCESS_WAIT".to_string())
            },
            bounded(stdout),
            bounded(stderr)
        )
    };
    let completed = {
        let operation = tokio::time::timeout(Duration::from_secs(30), operation);
        tokio::pin!(operation);
        let mut poll = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                result=&mut operation=>break result.map_err(|_|"GIT_PROCESS_TIMEOUT".to_string()).and_then(|v|v),
                _=poll.tick()=>{ if let Err(error)=scope.check() { break Err(error); } }
            }
        }
    };
    let (_, status, bytes, _stderr) = match completed {
        Ok(v) => v,
        Err(e) => {
            if child.try_wait().map_err(|_| "GIT_PROCESS_WAIT")?.is_none() {
                child.kill().await.map_err(|_| "GIT_PROCESS_TERMINATION")?;
            }
            return Err(e);
        }
    };
    scope.check()?;
    if !status.success() && missing_code.is_none_or(|code| status.code() != Some(code)) {
        return Err(format!(
            "GIT_COMMAND_FAILED: {}",
            status.code().unwrap_or(-1)
        ));
    }
    if !status.success() {
        return Ok(Vec::new());
    }
    Ok(bytes)
}
