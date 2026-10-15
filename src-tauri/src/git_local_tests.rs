use super::*;
use crate::{catalog, storage};
use std::{
    os::windows::{fs::MetadataExt, process::CommandExt},
    process::Command as StdCommand,
};

struct Fixture {
    core: Arc<Core>,
    root: PathBuf,
    path: PathBuf,
    identity: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-git-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
        let core = Arc::new(
            Core::new(
                root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let path = root.join("temp/repo");
        std::fs::create_dir(&path).unwrap();
        let f = Self {
            core,
            root,
            path,
            identity,
        };
        f.git(&["init", "--initial-branch=main", "--template=", "."]);
        f.git(&["config", "user.name", "Owned Fixture"]);
        f.git(&["config", "user.email", "owned@example.invalid"]);
        f.git(&["config", "commit.gpgsign", "false"]);
        f
    }
    fn git(&self, args: &[&str]) -> String {
        let mut cmd = StdCommand::new(executable().unwrap());
        cmd.env_clear();
        for key in ["SystemRoot", "WINDIR", "PATH", "USERPROFILE", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "gc.auto=0",
                "-c",
                "core.hooksPath=NUL",
                "-C",
            ])
            .arg(&self.path)
            .args(args)
            .creation_flags(0x08000000);
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "Owned setup failed: {:?}", args);
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    fn write(&self, name: &str, data: &str) {
        std::fs::write(self.path.join(name), data).unwrap();
    }
    fn staged(&self) {
        self.write("owned.txt", "original\n");
        self.git(&["add", "--", "owned.txt"]);
    }
    fn initial(&self) {
        self.staged();
        self.git(&["commit", "-m", "owned baseline"]);
    }
    async fn observation(&self) -> Value {
        self.core
            .execute("git.status", json!({"path":self.path}))
            .await
            .unwrap()["expected"]
            .clone()
    }
    async fn prepared(&self, tool: &str, extra: Value) -> Prepared {
        let mut input = json!({"path":self.path,"expected":self.observation().await});
        for (key, value) in extra.as_object().unwrap() {
            input[key] = value.clone();
        }
        prepare(&self.core, tool, &input).await.unwrap()
    }
    fn finish(self) {
        assert_eq!(
            self.root.parent().unwrap().canonicalize().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(self
            .root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ap-git-"));
        assert_eq!(
            asset_files::identity(&asset_files::directory(&self.root).unwrap()).unwrap(),
            self.identity
        );
        let mut dirs = vec![self.root.clone()];
        while let Some(path) = dirs.pop() {
            assert_eq!(
                std::fs::symlink_metadata(&path).unwrap().file_attributes() & 0x400,
                0
            );
            if path.is_dir() {
                dirs.extend(std::fs::read_dir(path).unwrap().map(|e| e.unwrap().path()));
            }
        }
        let Self { core, root, .. } = self;
        drop(core);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn strict_catalog_risk_and_branch_contract() {
    for tool in TOOLS {
        let schema = catalog::tool_schema(tool, "unused");
        let v = jsonschema::validator_for(&schema).unwrap();
        let mut input = json!({"path":"C:/owned","expected":{"fingerprint":"a".repeat(64)},"message":"Reviewed message","branch":"codex/owned"});
        input
            .as_object_mut()
            .unwrap()
            .retain(|key, _| schema["properties"].get(key).is_some());
        assert!(v.is_valid(&input), "{tool}");
        input["command"] = json!("arbitrary command");
        assert!(!v.is_valid(&input));
        let mut valid = json!({"path":"C:/owned"});
        if [
            "git.commit",
            "git.checkout",
            "git.stash",
            "git.delete_branch",
        ]
        .contains(tool)
        {
            assert!(!v.is_valid(&valid));
            assert_eq!(policy::classify(tool, &valid), policy::Risk::Mandatory);
        } else {
            assert_eq!(policy::classify(tool, &valid), policy::Risk::Free);
        }
        valid["limit"] = json!(101);
        if ["git.log", "git.commits"].contains(tool) {
            assert!(!v.is_valid(&valid));
        }
    }
    for name in [
        "-f",
        "@{-1}",
        "HEAD~1",
        "a..b",
        "a.lock",
        "a/.b",
        "a//b",
        "a/",
        "a space",
        "main;echo",
        "refs/heads/a.lock/b",
    ] {
        assert!(!branch(name), "{name}");
    }
    assert!(branch("codex/owned-1"));
}

#[tokio::test]
async fn owned_read_status_diff_branches_and_paged_commits() {
    let f = Fixture::new();
    let empty = f
        .core
        .execute("git.commits", json!({"path":f.path}))
        .await
        .unwrap();
    assert_eq!(empty["commits"], json!([]));
    f.initial();
    f.write("owned.txt", "edited Unicode \u{1f916}\n");
    let status = f
        .core
        .execute("git.status", json!({"path":f.path}))
        .await
        .unwrap();
    assert_eq!(status["branch"], "main");
    assert_eq!(status["has_working_changes"], true);
    let diff = f
        .core
        .execute("git.diff", json!({"path":f.path}))
        .await
        .unwrap();
    assert!(diff["patch"].as_str().unwrap().contains("\u{1f916}"));
    f.git(&["add", "owned.txt"]);
    f.git(&["commit", "-m", "second owned"]);
    let commits = f
        .core
        .execute("git.commits", json!({"path":f.path,"limit":1}))
        .await
        .unwrap();
    assert_eq!(commits["commits"].as_array().unwrap().len(), 1);
    assert_eq!(commits["next_offset"], 1);
    let next = f
        .core
        .execute("git.log", json!({"path":f.path,"limit":1,"offset":1}))
        .await
        .unwrap();
    assert_eq!(next["next_offset"], Value::Null);
    assert_eq!(next["commits"][0]["subject"], "owned baseline");
    let branches = f
        .core
        .execute("git.branches", json!({"path":f.path}))
        .await
        .unwrap();
    assert_eq!(branches["branches"][0]["name"], "main");
    f.finish();
}

#[tokio::test]
async fn reviewed_exact_staged_tree_commit_and_stale_index_denial() {
    let f = Fixture::new();
    f.staged();
    let p = f
        .prepared(
            "git.commit",
            json!({"message":"Reviewed Unicode \u{1f916}"}),
        )
        .await;
    let reviewed = p.snapshot.staged_tree.clone();
    let result = execute(p).await.unwrap();
    assert_eq!(result["tree"], reviewed);
    assert_eq!(result["pushed"], false);
    assert_eq!(f.git(&["rev-parse", "HEAD^{tree}"]), reviewed);
    assert!(f.git(&["status", "--porcelain"]).is_empty());
    f.write("owned.txt", "reviewed staged\n");
    f.git(&["add", "owned.txt"]);
    let p = f
        .prepared(
            "git.commit",
            json!({"message":"must not commit changed index"}),
        )
        .await;
    let old = f.git(&["rev-parse", "HEAD"]);
    f.write("owned.txt", "changed staged after review\n");
    f.git(&["add", "owned.txt"]);
    assert_eq!(execute(p).await.unwrap_err(), "GIT_OBSERVATION_STALE");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), old);
    f.finish();
}

#[tokio::test]
async fn stash_snapshot_retains_worktree_index_and_untracked_files() {
    let f = Fixture::new();
    f.initial();
    f.write("owned.txt", "staged version\n");
    f.git(&["add", "owned.txt"]);
    f.write("owned.txt", "working version\n");
    f.write("untracked.txt", "untouched\n");
    let index = std::fs::read(f.path.join(".git/index")).unwrap();
    let head = f.git(&["rev-parse", "HEAD"]);
    let p = f.prepared("git.stash", json!({})).await;
    let index_after_prepare = std::fs::read(f.path.join(".git/index")).unwrap();
    let result = execute(p).await.unwrap();
    assert_eq!(result["working_tree_retained"], true);
    assert_eq!(result["index_retained"], true);
    assert_eq!(
        std::fs::read_to_string(f.path.join("owned.txt")).unwrap(),
        "working version\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.path.join("untracked.txt")).unwrap(),
        "untouched\n"
    );
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(f.git(&["show", ":owned.txt"]), "staged version");
    assert_eq!(
        std::fs::read(f.path.join(".git/index")).unwrap(),
        index_after_prepare
    );
    assert!(!index.is_empty());
    assert_eq!(f.git(&["rev-parse", "refs/stash"]), result["stash_oid"]);
    f.finish();
}

#[tokio::test]
async fn clean_checkout_and_merged_branch_delete_without_hooks() {
    let f = Fixture::new();
    f.initial();
    f.git(&["branch", "owned-next"]);
    let marker = f.path.join("hook-ran");
    std::fs::create_dir(f.path.join(".git/hooks")).unwrap();
    f.write(
        ".git/hooks/post-checkout",
        &format!("#!/bin/sh\necho unexpected > '{}'\n", marker.display()),
    );
    let p = f
        .prepared("git.checkout", json!({"branch":"owned-next"}))
        .await;
    execute(p).await.unwrap();
    assert_eq!(f.git(&["branch", "--show-current"]), "owned-next");
    assert!(!marker.exists());
    f.write("owned.txt", "dirty\n");
    let expected = f.observation().await;
    assert_eq!(
        prepare(
            &f.core,
            "git.checkout",
            &json!({"path":f.path,"branch":"main","expected":expected})
        )
        .await
        .err()
        .unwrap(),
        "GIT_CLEAN_WORKTREE_REQUIRED"
    );
    f.write("owned.txt", "original\n");
    let p = f
        .prepared("git.delete_branch", json!({"branch":"main"}))
        .await;
    execute(p).await.unwrap();
    assert_eq!(f.git(&["branch", "--list", "main"]), "");
    assert_eq!(f.git(&["branch", "--show-current"]), "owned-next");
    f.finish();
}

#[tokio::test]
async fn unmerged_and_worktree_branch_delete_denied() {
    let f = Fixture::new();
    f.initial();
    f.git(&["branch", "owned-other"]);
    f.git(&["switch", "owned-other"]);
    f.write("owned.txt", "other\n");
    f.git(&["add", "owned.txt"]);
    f.git(&["commit", "-m", "other commit"]);
    f.git(&["switch", "main"]);
    let p = f
        .prepared("git.delete_branch", json!({"branch":"owned-other"}))
        .await;
    assert_eq!(execute(p).await.unwrap_err(), "GIT_BRANCH_NOT_MERGED");
    f.git(&["branch", "owned-active"]);
    let second = f.root.join("temp/second");
    f.git(&["worktree", "add", second.to_str().unwrap(), "owned-active"]);
    let p = f
        .prepared("git.delete_branch", json!({"branch":"owned-active"}))
        .await;
    assert_eq!(execute(p).await.unwrap_err(), "GIT_BRANCH_IN_WORKTREE");
    let read = f
        .core
        .execute("git.status", json!({"path":second}))
        .await
        .unwrap();
    assert_eq!(read["branch"], "owned-active");
    f.finish();
}

#[tokio::test]
async fn scope_privacy_disable_inflight_and_parent_repo_denied() {
    let f = Fixture::new();
    f.initial();
    std::fs::create_dir(f.path.join("child")).unwrap();
    assert_eq!(
        f.core
            .execute("git.status", json!({"path":f.path.join("child")}))
            .await
            .unwrap_err(),
        "GIT_WORKTREE_REQUIRED"
    );
    f.write("owned.txt", "staged\n");
    f.git(&["add", "owned.txt"]);
    let p = f
        .prepared("git.commit", json!({"message":"privacy denied"}))
        .await;
    let before = f.git(&["rev-parse", "HEAD"]);
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(execute(p).await.unwrap_err(), "PRIVACY_DENIED");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), before);
    f.core.store.set_setting("privacy", &json!(false)).unwrap();
    let p = f
        .prepared("git.commit", json!({"message":"disabled denied"}))
        .await;
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=0 WHERE module_id='git'",
            &[],
        )
        .unwrap();
    assert_eq!(execute(p).await.unwrap_err(), "MODULE_DISABLED: git");
    f.core
        .store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='git'",
            &[],
        )
        .unwrap();
    let p = f
        .prepared("git.commit", json!({"message":"changed scope denied"}))
        .await;
    f.core
        .store
        .set_setting("allowed_roots", &json!([f.root.join("temp")]))
        .unwrap();
    assert_eq!(execute(p).await.unwrap_err(), "GIT_SCOPE_CHANGED");
    f.finish();
}

#[tokio::test]
async fn filters_includes_partialclone_operation_and_signing_fail_closed() {
    let f = Fixture::new();
    f.staged();
    for (key, value, error) in [
        (
            "filter.owned.clean",
            "echo should-not-run",
            "GIT_FILTER_EXECUTION_DENIED",
        ),
        (
            "include.path",
            "owned-config",
            "GIT_PARTIAL_OR_REDIRECTED_REPOSITORY_DENIED",
        ),
        (
            "remote.origin.promisor",
            "true",
            "GIT_PARTIAL_OR_REDIRECTED_REPOSITORY_DENIED",
        ),
    ] {
        f.git(&["config", key, value]);
        assert_eq!(
            f.core
                .execute("git.status", json!({"path":f.path}))
                .await
                .unwrap_err(),
            error
        );
        f.git(&["config", "--unset", key]);
    }
    f.write(
        ".gitattributes",
        "*.txt filter=unconfigured-global-filter\n",
    );
    assert_eq!(
        f.core
            .execute("git.status", json!({"path":f.path}))
            .await
            .unwrap_err(),
        "GIT_FILTER_EXECUTION_DENIED"
    );
    std::fs::remove_file(f.path.join(".gitattributes")).unwrap();
    f.git(&["config", "commit.gpgsign", "true"]);
    let expected = f.observation().await;
    assert_eq!(
        prepare(
            &f.core,
            "git.commit",
            &json!({"path":f.path,"message":"unsigned denied","expected":expected})
        )
        .await
        .err()
        .unwrap(),
        "GIT_SIGNED_COMMIT_REQUIRED_UNSUPPORTED"
    );
    f.git(&["config", "commit.gpgsign", "false"]);
    let expected = f.observation().await;
    f.write(".git/MERGE_HEAD", &"a".repeat(40));
    assert_eq!(
        prepare(
            &f.core,
            "git.commit",
            &json!({"path":f.path,"message":"merge denied","expected":expected})
        )
        .await
        .err()
        .unwrap(),
        "GIT_OPERATION_IN_PROGRESS"
    );
    f.finish();
}

#[tokio::test]
async fn core_permission_denial_and_audit_omit_patch_message() {
    let f = Fixture::new();
    f.staged();
    let expected = f.observation().await;
    assert_eq!(
        f.core
            .execute(
                "git.commit",
                json!({"path":f.path,"message":"private proposed message","expected":expected})
            )
            .await
            .unwrap_err(),
        "APP_MISSING"
    );
    let durable = f
        .core
        .store
        .query(
            "SELECT input_json_redacted FROM permission_requests WHERE tool_id='git.commit'",
            &[],
        )
        .unwrap()
        .to_string();
    assert!(!durable.contains("private proposed message"));
    assert!(!durable.contains("+original"));
    assert!(durable.contains("GIT_CONTENT_OMITTED"));
    assert!(f.core.permission_previews.lock().unwrap().is_empty());
    assert_eq!(f.git(&["rev-parse", "--is-inside-work-tree"]), "true");
    let audited = policy::tool_redact(
        "git.diff",
        &json!({"patch":"unknown private source", "nested":{"message":"unknown private message"}}),
    );
    assert!(!audited.to_string().contains("unknown private"));
    f.finish();
}

#[tokio::test]
async fn native_output_bound_and_sensitive_patch_rejected_without_mutation() {
    let f = Fixture::new();
    f.initial();
    let head = f.git(&["rev-parse", "HEAD"]);
    f.write("owned.txt", &"x".repeat(OUTPUT_LIMIT + 65536));
    assert_eq!(
        f.core
            .execute("git.status", json!({"path":f.path}))
            .await
            .unwrap_err(),
        "GIT_OUTPUT_LIMIT"
    );
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    f.write("owned.txt", "password = fictional-fixture-value\n");
    assert_eq!(
        f.core
            .execute("git.diff", json!({"path":f.path}))
            .await
            .unwrap_err(),
        "GIT_SENSITIVE_CONTENT_DENIED"
    );
    f.git(&["add", "owned.txt"]);
    let expected = f.observation().await;
    assert_eq!(
        prepare(
            &f.core,
            "git.commit",
            &json!({"path":f.path,"message":"sensitive staged denied","expected":expected})
        )
        .await
        .err()
        .unwrap(),
        "GIT_SENSITIVE_CONTENT_DENIED"
    );
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    f.finish();
}

#[tokio::test]
async fn owned_native_process_privacy_revocation_terminates_before_timeout() {
    let f = Fixture::new();
    let ready = f.root.join("owned-process-ready");
    let scope = Scope {
        core: f.core.clone(),
        roots: f.core.roots(),
    };
    let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut cmd = Command::new(shell);
    cmd.env_clear()
        .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
        .env("ALFREDPET_OWNED_READY", &ready)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[IO.File]::WriteAllText($env:ALFREDPET_OWNED_READY,'ready'); Start-Sleep -Seconds 30",
        ]);
    let task = tokio::spawn(async move { run_command(&scope, cmd, None, None).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "owned readiness deadline"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let now = tokio::time::Instant::now();
    f.core.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(task.await.unwrap().unwrap_err(), "PRIVACY_DENIED");
    assert!(now.elapsed() < Duration::from_secs(5));
    f.finish();
}

#[tokio::test]
async fn hardlinked_worktree_and_shared_object_store_are_denied() {
    let f = Fixture::new();
    f.initial();
    let alias = f.root.join("owned-outside-alias");
    std::fs::hard_link(f.path.join("owned.txt"), &alias).unwrap();
    assert_eq!(
        f.core
            .execute("git.status", json!({"path":f.path}))
            .await
            .unwrap_err(),
        "GIT_FILE_LINK_OR_ACCESS_DENIED"
    );
    std::fs::remove_file(alias).unwrap();
    std::fs::create_dir_all(f.path.join(".git/objects/info")).unwrap();
    f.write(
        ".git/objects/info/alternates",
        "fictional-outside-object-store\n",
    );
    assert_eq!(
        f.core
            .execute("git.status", json!({"path":f.path}))
            .await
            .unwrap_err(),
        "GIT_SHARED_OR_GRAFTED_OBJECT_STORE_DENIED"
    );
    f.finish();
}

#[tokio::test]
async fn imported_line_endings_preserve_owned_clean_crlf_repository() {
    let f = Fixture::new();
    f.git(&["config", "core.autocrlf", "true"]);
    f.git(&["config", "core.eol", "crlf"]);
    f.write("owned.txt", "original\r\n");
    let owned_file = f.path.join("owned.txt");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&owned_file)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(10)),
        )
        .unwrap();
    f.git(&["add", "owned.txt"]);
    f.git(&["commit", "-m", "owned CRLF"]);
    let scope = Scope {
        core: f.core.clone(),
        roots: f.core.roots(),
    };
    let mut repo = repository(&scope, f.path.to_str().unwrap()).await.unwrap();
    assert!(repo
        .safe_config
        .contains(&("core.autocrlf", "true".to_string())));
    assert_eq!(snapshot(&repo, &scope).await.unwrap().status, "");
    // Emulate removing the original config layer after importing its safe
    // values, with owned local settings only. Never alter the user's globals.
    repo._leases.clear();
    f.git(&["config", "core.autocrlf", "false"]);
    f.git(&["config", "--unset", "core.eol"]);
    assert_eq!(snapshot(&repo, &scope).await.unwrap().status, "");
    repo.safe_config.clear();
    // Changing only config can retain a clean stat cache. Force this negative
    // control to read the unchanged CRLF bytes instead of depending on timing.
    assert_eq!(snapshot(&repo, &scope).await.unwrap().status, "");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&owned_file)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() + Duration::from_secs(2)),
        )
        .unwrap();
    assert!(!snapshot(&repo, &scope).await.unwrap().status.is_empty());
    assert_eq!(std::fs::read(&owned_file).unwrap(), b"original\r\n");
    repo.safe_config = vec![
        ("core.autocrlf", "true".into()),
        ("core.eol", "crlf".into()),
    ];
    assert_eq!(snapshot(&repo, &scope).await.unwrap().status, "");
    assert_eq!(std::fs::read(&owned_file).unwrap(), b"original\r\n");
    assert!(safe_config_value("core.eol", "arbitrary command").is_err());
    assert!(safe_config_value("core.autocrlf", "execute hook").is_err());
    drop(repo);
    drop(scope);
    f.finish();
}
