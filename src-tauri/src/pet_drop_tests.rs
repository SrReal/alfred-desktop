use super::*;
use crate::asset_files;
use std::{os::windows::fs::MetadataExt, time::Duration};
struct Fixture {
    c: Arc<Core>,
    root: PathBuf,
    identity: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ap-pet-drop-{}", storage::id()));
        std::fs::create_dir(&root).unwrap();
        let identity = asset_files::identity(&asset_files::directory(&root).unwrap()).unwrap();
        let c = Arc::new(
            Core::new(
                root.clone(),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        c.store
            .set_setting("allowed_roots", &json!([root]))
            .unwrap();
        Self { c, root, identity }
    }
    fn file(&self, name: &str) -> PathBuf {
        let p = self.root.join(name);
        std::fs::write(&p, "BM owned drop fixture\n").unwrap();
        p
    }
    fn conversation(&self) -> String {
        let id = storage::id();
        self.c.store.execute("INSERT INTO conversations(id,title,created_at,updated_at) VALUES(?,'Owned drop fixture',?,?)",&[json!(id),json!(storage::now()),json!(storage::now())]).unwrap();
        id
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
            .starts_with("ap-pet-drop-"));
        assert_eq!(
            asset_files::identity(&asset_files::directory(&self.root).unwrap()).unwrap(),
            self.identity
        );
        let mut todo = vec![self.root.clone()];
        while let Some(p) = todo.pop() {
            assert_eq!(
                std::fs::symlink_metadata(&p).unwrap().file_attributes() & 0x400,
                0
            );
            if p.is_dir() {
                todo.extend(std::fs::read_dir(p).unwrap().map(|e| e.unwrap().path()));
            }
        }
        drop(self.c);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
#[tokio::test]
async fn target_bounds_privacy_and_untrusted_rpc_paths_do_not_queue_or_attach() {
    let f = Fixture::new();
    let p = f.file("owned.txt");
    for (surface, bounds) in [("chat", true), ("pet", false)] {
        assert_eq!(
            receive(&f.c, surface, bounds, std::slice::from_ref(&p))
                .await
                .unwrap_err(),
            "PET_DROP_TARGET_REQUIRED"
        );
    }
    assert!(inside(87.0, 103.0, 88, 104));
    for (x, y) in [(-1.0, 0.0), (88.0, 0.0), (0.0, 104.0), (f64::NAN, 0.0)] {
        assert!(!inside(x, y, 88, 104));
    }
    f.c.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        receive(&f.c, "pet", true, &[p]).await.unwrap_err(),
        "PRIVACY_DENIED"
    );
    assert!(list(&f.c).unwrap()["items"].as_array().unwrap().is_empty());
    assert_eq!(
        dispatch(
            &f.c,
            "pet_drop.attach",
            &json!({"ids":[storage::id()],"path":"not an OS drop","conversation_id":storage::id()})
        )
        .await
        .unwrap_err(),
        "DROP_INPUT"
    );
    f.finish();
}
#[tokio::test]
async fn drop_choice_survives_restart_and_only_explicit_attach_copies_bytes() {
    let f = Fixture::new();
    let p = f.file("owned.txt");
    let conversation = f.conversation();
    let received = receive(&f.c, "pet", true, &[p.clone(), p.clone()])
        .await
        .unwrap();
    assert_eq!(received["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        f.c.store
            .query("SELECT COUNT(*) AS n FROM asset_records", &[])
            .unwrap()[0]["n"],
        0
    );
    let restarted = Arc::new(Core::new(f.root.clone(), f.c.resource.clone()).unwrap());
    let pending = list(&restarted).unwrap();
    let id = pending["items"][0]["id"].clone();
    let result = dispatch(
        &restarted,
        "pet_drop.attach",
        &json!({"ids":[id],"conversation_id":conversation}),
    )
    .await
    .unwrap();
    assert_eq!(result["attached"].as_array().unwrap().len(), 1);
    assert!(result["failed"].as_array().unwrap().is_empty());
    let copy = PathBuf::from(result["attached"][0]["path"].as_str().unwrap());
    assert_ne!(copy, p);
    assert_eq!(std::fs::read(&copy).unwrap(), std::fs::read(&p).unwrap());
    assert!(list(&restarted).unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        restarted
            .store
            .query("SELECT COUNT(*) AS n FROM messages", &[])
            .unwrap()[0]["n"],
        0
    );
    drop(restarted);
    f.finish();
}
#[tokio::test]
async fn partial_attachment_failures_remain_visible_and_can_be_retried_or_dismissed() {
    let f = Fixture::new();
    let a = f.file("a.txt");
    let b = f.file("b.txt");
    let conversation = f.conversation();
    let q = receive(&f.c, "pet", true, &[a.clone(), b.clone()])
        .await
        .unwrap();
    let ids = vec![q["items"][0]["id"].clone(), q["items"][1]["id"].clone()];
    std::fs::remove_file(&b).unwrap();
    let r = dispatch(
        &f.c,
        "pet_drop.attach",
        &json!({"ids":ids,"conversation_id":conversation}),
    )
    .await
    .unwrap();
    assert_eq!(r["attached"].as_array().unwrap().len(), 1);
    assert_eq!(r["failed"].as_array().unwrap().len(), 1);
    let pending = list(&f.c).unwrap();
    assert!(pending["items"][0]["error"].is_string());
    let id = pending["items"][0]["id"].clone();
    std::fs::write(&b, b"recreated owned text").unwrap();
    let r = dispatch(
        &f.c,
        "pet_drop.attach",
        &json!({"ids":[id],"conversation_id":conversation}),
    )
    .await
    .unwrap();
    assert_eq!(r["attached"].as_array().unwrap().len(), 1);
    let p = f.file("dismiss.txt");
    let q = receive(&f.c, "pet", true, std::slice::from_ref(&p))
        .await
        .unwrap();
    dispatch(
        &f.c,
        "pet_drop.dismiss",
        &json!({"ids":[q["items"][0]["id"]]}),
    )
    .await
    .unwrap();
    assert!(p.exists());
    assert!(a.exists());
    assert!(b.exists());
    f.finish();
}
#[tokio::test]
async fn durable_attachment_receipt_prevents_duplicate_copy_after_queue_commit_interruption() {
    let f = Fixture::new();
    let p = f.file("receipt.txt");
    let conversation = f.conversation();
    let other = f.conversation();
    let q = receive(&f.c, "pet", true, std::slice::from_ref(&p))
        .await
        .unwrap();
    let id = q["items"][0]["id"].as_str().unwrap();
    let input = json!({"conversation_id":conversation,"path":p});
    let first = crate::assets::attach_from_drop(&f.c, &input, id)
        .await
        .unwrap();
    std::fs::write(&p, b"later source edit must not replace receipt").unwrap();
    let second = crate::assets::attach_from_drop(&f.c, &input, id)
        .await
        .unwrap();
    assert_eq!(first["id"], second["id"]);
    assert_eq!(second["already_attached"], true);
    assert_eq!(
        crate::assets::attach_from_drop(&f.c, &json!({"conversation_id":other,"path":p}), id)
            .await
            .unwrap_err(),
        "DROP_ALREADY_ATTACHED_OTHER_CONVERSATION"
    );
    assert_eq!(
        f.c.store
            .query("SELECT COUNT(*) AS n FROM attachments", &[])
            .unwrap()[0]["n"],
        1
    );
    assert_eq!(
        f.c.store
            .query("SELECT COUNT(*) AS n FROM asset_records", &[])
            .unwrap()[0]["n"],
        1
    );
    assert!(list(&f.c).unwrap()["items"].as_array().unwrap().is_empty());
    f.finish();
}
#[tokio::test]
async fn independent_profile_actors_append_without_losing_other_pending_files() {
    let f = Fixture::new();
    let p = f.file("one.txt");
    let second = f.file("two.txt");
    let c = Arc::new(Core::new(f.root.clone(), f.c.resource.clone()).unwrap());
    let first = {
        let c = f.c.clone();
        tokio::spawn(async move { receive(&c, "pet", true, &[p]).await })
    };
    let other = {
        let c = c.clone();
        tokio::spawn(async move { receive(&c, "pet", true, &[second]).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        first.await.unwrap().unwrap();
        other.await.unwrap().unwrap();
    })
    .await
    .unwrap();
    assert_eq!(list(&f.c).unwrap()["items"].as_array().unwrap().len(), 2);
    drop(c);
    f.finish();
}

#[tokio::test]
async fn scope_and_privacy_are_rechecked_after_drop_and_before_copy() {
    let f = Fixture::new();
    let p = f.file("revoked.txt");
    let conversation = f.conversation();
    let q = receive(&f.c, "pet", true, std::slice::from_ref(&p))
        .await
        .unwrap();
    let id = q["items"][0]["id"].clone();
    f.c.store.set_setting("allowed_roots", &json!([])).unwrap();
    let r = dispatch(
        &f.c,
        "pet_drop.attach",
        &json!({"ids":[id],"conversation_id":conversation}),
    )
    .await
    .unwrap();
    assert!(r["attached"].as_array().unwrap().is_empty());
    assert!(r["failed"][0]["error"]
        .as_str()
        .unwrap()
        .starts_with("PATH_SCOPE"));
    f.c.store
        .set_setting("allowed_roots", &json!([f.root]))
        .unwrap();
    f.c.store.set_setting("privacy", &json!(true)).unwrap();
    assert_eq!(
        dispatch(
            &f.c,
            "pet_drop.attach",
            &json!({"ids":[id],"conversation_id":conversation})
        )
        .await
        .unwrap_err(),
        "PRIVACY_DENIED"
    );
    assert_eq!(
        crate::assets::attach_from_drop(
            &f.c,
            &json!({"path":p,"conversation_id":conversation}),
            id.as_str().unwrap()
        )
        .await
        .unwrap_err(),
        "PRIVACY_DENIED"
    );
    assert_eq!(
        f.c.store
            .query("SELECT COUNT(*) AS n FROM attachments", &[])
            .unwrap()[0]["n"],
        0
    );
    assert!(p.exists());
    f.finish();
}
