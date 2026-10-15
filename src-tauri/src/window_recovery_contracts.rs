use super::*;

#[tokio::test]
async fn native_watches_observe_only_the_owned_process_and_window_lifecycle() {
    let f = Fixture::new();
    let windows = Windows::new(&f.core).await;
    let row = windows.info["windows"][0].clone();
    let pid = row["process_id"].as_u64().unwrap() as u32;
    let hwnd = row["hwnd"].as_i64().unwrap();
    let process = crate::windows_native::watch_process(pid).unwrap();
    assert_eq!(process["present"], true);
    assert_eq!(process["process_id"], pid);
    // Independent .NET fixture reports its own creation time in ticks.
    assert_eq!(
        process["started"].as_u64().unwrap().to_string(),
        row["process_started"].as_str().unwrap()
    );
    assert_eq!(
        crate::windows_native::watch_window(hwnd).unwrap(),
        json!({"present":true,"process_id":pid,"visible":true})
    );
    assert_eq!(
        crate::windows_native::watch_process(0).unwrap_err(),
        "WATCH_TARGET_ID"
    );
    assert_eq!(
        crate::windows_native::watch_window(0).unwrap_err(),
        "WATCH_TARGET_ID"
    );
    windows.finish().await;
    assert_eq!(
        crate::windows_native::watch_process(pid).unwrap(),
        json!({"present":false})
    );
    assert_eq!(
        crate::windows_native::watch_window(hwnd).unwrap(),
        json!({"present":false})
    );
    f.finish();
}

#[tokio::test]
async fn real_saved_layout_undo_survives_layout_deletion_and_state_restore_preserves_latent_maximize(
) {
    let f = Fixture::new();
    let mut windows = Windows::new(&f.core).await;
    let targets: Vec<Value> = windows.info["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| json!({"hwnd":w["hwnd"],"process_id":w["process_id"]}))
        .collect();
    let saved = window_layouts::dispatch(
        &f.core,
        "layout_save",
        &json!({"name":"Owned layout undo","body":targets}),
    )
    .await
    .unwrap();
    let mut original = Vec::new();
    let mut placements = Vec::new();
    for target in &targets {
        let before = current(&f.core, target).await;
        let mut desired = before.clone();
        desired["x"] = json!(number(&before, "x").unwrap() + 25);
        original.push(before);
        placements.push(desired);
    }
    let moved = change(&f.core, "window.set_layout", &json!({"body":placements})).await;
    let mut observed = Vec::new();
    for target in &targets {
        observed.push(current(&f.core, target).await);
    }
    let selection = json!({"id":saved["id"],"fingerprint":saved["fingerprint"]});
    let applied = window_layouts::dispatch(&f.core, "layout_apply", &selection)
        .await
        .unwrap();
    for (target, before) in targets.iter().zip(&original) {
        assert_eq!(current(&f.core, target).await, *before);
    }
    assert_eq!(
        crate::policy::classify("window.layout_delete", &selection),
        crate::policy::Risk::Mandatory
    );
    window_layouts::dispatch(&f.core, "layout_delete", &selection)
        .await
        .unwrap();
    let restarted = Arc::new(Core::new(f.core.root.clone(), f.core.resource.clone()).unwrap());
    undo_native(&restarted, &applied).await;
    for (target, after) in targets.iter().zip(&observed) {
        assert_eq!(current(&restarted, target).await, *after);
    }
    undo_native(&restarted, &moved).await;
    for (target, before) in targets.iter().zip(&original) {
        assert_eq!(current(&restarted, target).await, *before);
    }
    let saved = window_layouts::dispatch(
        &restarted,
        "layout_save",
        &json!({"name":"Owned preflight","body":targets}),
    )
    .await
    .unwrap();
    let second = change(
        &restarted,
        "window.state",
        &json!({"hwnd":targets[1]["hwnd"],"state":"minimize"}),
    )
    .await;
    // A saved layout must still reject a minimized target before persisting anything.
    assert_eq!(
        window_layouts::dispatch(
            &restarted,
            "layout_save",
            &json!({"name":"Rejected minimized layout","body":targets}),
        )
        .await
        .unwrap_err(),
        "LAYOUT_WINDOW_NOT_NORMAL"
    );
    let first = change(&restarted, "window.move", &placements[0]).await;
    let preflight_before = [
        current(&restarted, &targets[0]).await,
        current(&restarted, &targets[1]).await,
    ];
    let records = history(&restarted).unwrap().as_array().unwrap().len();
    let selection = json!({"id":saved["id"],"fingerprint":saved["fingerprint"]});
    let error = match prepare(&restarted, "window.layout_apply", &selection).await {
        Ok(_) => panic!("non-normal target must reject the whole layout"),
        Err(error) => error,
    };
    assert_eq!(error, "LAYOUT_WINDOW_NOT_NORMAL");
    assert_eq!(
        history(&restarted).unwrap().as_array().unwrap().len(),
        records
    );
    for (target, before) in targets.iter().zip(&preflight_before) {
        assert_eq!(current(&restarted, target).await, *before);
    }
    undo_native(&restarted, &first).await;
    undo_native(&restarted, &second).await;
    window_layouts::dispatch(&restarted, "layout_delete", &selection)
        .await
        .unwrap();
    let target = &targets[0];
    for maximized in [false, true] {
        let max = if maximized {
            Some(
                change(
                    &restarted,
                    "window.state",
                    &json!({"hwnd":target["hwnd"],"state":"maximize"}),
                )
                .await,
            )
        } else {
            None
        };
        if maximized {
            let before_max = current(&restarted, target).await;
            let restored = change(
                &restarted,
                "window.state",
                &json!({"hwnd":target["hwnd"],"state":"restore"}),
            )
            .await;
            assert_eq!(
                current(&restarted, target).await["placement"]["show_cmd"],
                1
            );
            undo_native(&restarted, &restored).await;
            assert_eq!(current(&restarted, target).await, before_max);
        }
        let minimized = change(
            &restarted,
            "window.state",
            &json!({"hwnd":target["hwnd"],"state":"minimize"}),
        )
        .await;
        let before = current(&restarted, target).await;
        windows.command("report").await;
        assert_eq!(
            windows.info["windows"][0]["placement_flags"]
                .as_u64()
                .unwrap()
                & 2,
            if maximized { 2 } else { 0 }
        );
        assert_eq!(before["placement"]["restore_maximized"] == true, maximized);
        let restored = change(
            &restarted,
            "window.state",
            &json!({"hwnd":target["hwnd"],"state":"restore"}),
        )
        .await;
        assert_eq!(
            current(&restarted, target).await["placement"]["show_cmd"],
            if maximized { 3 } else { 1 }
        );
        undo_native(&restarted, &restored).await;
        assert_eq!(current(&restarted, target).await, before);
        if maximized {
            let snap = change(
                &restarted,
                "window.snap",
                &json!({"hwnd":target["hwnd"],"kind":"top_left"}),
            )
            .await;
            undo_native(&restarted, &snap).await;
            assert_eq!(current(&restarted, target).await, before);
            // Verify latent restore behavior through the actual state command.
            let restored = change(
                &restarted,
                "window.state",
                &json!({"hwnd":target["hwnd"],"state":"restore"}),
            )
            .await;
            assert_eq!(
                current(&restarted, target).await["placement"]["show_cmd"],
                3
            );
            undo_native(&restarted, &restored).await;
        }
        undo_native(&restarted, &minimized).await;
        if let Some(max) = max {
            undo_native(&restarted, &max).await;
        }
        assert_eq!(current(&restarted, target).await, original[0]);
    }
    windows.finish().await;
    drop(restarted);
    f.finish();
    println!("WINDOW_STATE_LAYOUT_UNDO_PASS: two owned windows/saved apply persistent undo after layout deletion/whole-set normal-state preflight/direct restore undo/minimized-from-maximized latent restore preserved; no human L3 approval or GUI crash certified");
}

#[path = "window_process_kill_contracts.rs"]
pub(in crate::window_undo) mod crash;

#[test]
fn legacy_minimized_before_images_require_latent_state_evidence() {
    let f = Fixture::new();
    let mut legacy = fake_record();
    legacy.steps[0].before["placement"]["show_cmd"] = json!(2);
    let id = insert(&f.core, &legacy, "blocked");
    assert_eq!(
        load(&f.core, &id).unwrap_err(),
        "WINDOW_LEGACY_MINIMIZED_STATE_UNKNOWN: manual review required"
    );
    legacy.version = 2;
    let current_id = insert(&f.core, &legacy, "blocked");
    assert!(load(&f.core, &current_id).is_ok());
    legacy.version = 1;
    legacy.steps[0].before["placement"]["restore_maximized"] = json!(true);
    let known = insert(&f.core, &legacy, "blocked");
    assert!(load(&f.core, &known).is_ok());
    legacy.steps[0].before["placement"]["show_cmd"] = json!(1);
    assert_eq!(
        snapshot(&legacy.steps[0].before, true).unwrap_err(),
        "WINDOW_SNAPSHOT_STATE"
    );
    f.finish();
}
