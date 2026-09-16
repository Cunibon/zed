use std::{path::Path, sync::Arc, time::Duration};

use collections::{BTreeSet, HashSet};
use fs::FakeFs;
use futures::{FutureExt as _, StreamExt as _};
use gpui::{Entity, FutureExt as _, UpdateGlobal as _};
use language::{FakeLspAdapter, LanguageName, rust_lang};
use lsp::LanguageServerName;
use pretty_assertions::{assert_eq, assert_matches};
use project::{LspStore, LspStoreEvent, Project};
use serde_json::json;
use settings::{GlobalLspSettingsContent, SettingsStore};
use util::{path, uri};

use crate::{init_test, js_lang};

#[gpui::test]
async fn test_stop_all_language_servers_is_reversible(cx: &mut gpui::TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir"), json!({ "a.rs": "", "b.rs": "" }))
        .await;

    let project = Project::test(fs, [path!("/dir").as_ref()], cx).await;
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());

    let mut fake_rust_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "rust-lsp",
            ..Default::default()
        },
    );
    language_registry.add(rust_lang());

    let (buffer_a, _handle_a) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_1 = fake_rust_servers.next().await.unwrap();
    assert_eq!(
        fake_rust_server_1
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .await
            .text_document
            .uri
            .as_str(),
        uri!("file:///dir/a.rs")
    );

    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.stop_all_language_servers(cx);
    });
    fake_rust_server_1
        .receive_notification::<lsp::notification::Exit>()
        .await;

    // Opening another buffer of the same language must respect the stop:
    // no server may be started until the user explicitly asks for one.
    let (_buffer_b, _handle_b) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/b.rs"), cx)
        })
        .await
        .unwrap();
    cx.executor().run_until_parked();
    assert!(
        fake_rust_servers.next().now_or_never().flatten().is_none(),
        "no language server should start while all servers are stopped"
    );

    // `editor::RestartLanguageServer` restarts the servers of the editor's buffers
    // with `clear_stopped`, which must revive servers after "Stop All".
    project.update(cx, |project, cx| {
        project.restart_language_servers_for_buffers(
            vec![buffer_a.clone()],
            HashSet::default(),
            true,
            cx,
        );
    });
    let mut fake_rust_server_2 = fake_rust_servers
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("a language server should start after restarting with clear_stopped")
        .unwrap();
    let opened_after_restart = BTreeSet::from_iter([
        fake_rust_server_2
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .await
            .text_document
            .uri
            .to_string(),
        fake_rust_server_2
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .await
            .text_document
            .uri
            .to_string(),
    ]);
    assert_eq!(
        opened_after_restart,
        BTreeSet::from_iter([
            uri!("file:///dir/a.rs").to_string(),
            uri!("file:///dir/b.rs").to_string(),
        ])
    );

    // A buffer-independent start (what the LSP menu's "Start Server" uses) must
    // revive a server after "Stop All" as well.
    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.stop_all_language_servers(cx);
    });
    fake_rust_server_2
        .receive_notification::<lsp::notification::Exit>()
        .await;
    let worktree_id = project.read_with(cx, |project, cx| {
        project.worktrees(cx).next().unwrap().read(cx).id()
    });
    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.start_language_server_for(
            worktree_id,
            LanguageServerName::new_static("rust-lsp"),
            cx,
        );
    });
    let mut fake_rust_server_3 = fake_rust_servers
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("a language server should start after start_language_server_for")
        .unwrap();
    let opened_after_start = BTreeSet::from_iter([
        fake_rust_server_3
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .await
            .text_document
            .uri
            .to_string(),
        fake_rust_server_3
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .await
            .text_document
            .uri
            .to_string(),
    ]);
    assert_eq!(
        opened_after_start,
        BTreeSet::from_iter([
            uri!("file:///dir/a.rs").to_string(),
            uri!("file:///dir/b.rs").to_string(),
        ])
    );
}

#[gpui::test]
async fn test_starting_one_server_after_stop_all_keeps_others_stopped(
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir"), json!({ "a.rs": "", "b.js": "" }))
        .await;

    let project = Project::test(fs, [path!("/dir").as_ref()], cx).await;
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());

    let mut fake_rust_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "rust-lsp",
            ..Default::default()
        },
    );
    let mut fake_js_servers = language_registry.register_fake_lsp(
        "JavaScript",
        FakeLspAdapter {
            name: "js-lsp",
            ..Default::default()
        },
    );
    language_registry.add(rust_lang());
    language_registry.add(js_lang());

    let _rs_buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/a.rs"), cx)
        })
        .await
        .unwrap();
    let _js_buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/b.js"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_1 = fake_rust_servers.next().await.unwrap();
    fake_rust_server_1
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    let mut fake_js_server_1 = fake_js_servers.next().await.unwrap();
    fake_js_server_1
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.stop_all_language_servers(cx);
    });
    fake_rust_server_1
        .receive_notification::<lsp::notification::Exit>()
        .await;
    fake_js_server_1
        .receive_notification::<lsp::notification::Exit>()
        .await;

    let worktree_id = project.read_with(cx, |project, cx| {
        project.worktrees(cx).next().unwrap().read(cx).id()
    });
    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.start_language_server_for(
            worktree_id,
            LanguageServerName::new_static("rust-lsp"),
            cx,
        );
    });
    let mut fake_rust_server_2 = fake_rust_servers
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("the started server should come up")
        .unwrap();
    fake_rust_server_2
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    cx.executor().run_until_parked();
    assert!(
        fake_js_servers.next().now_or_never().flatten().is_none(),
        "starting one server must not revive the other stopped server"
    );

    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.restart_all_language_servers(cx);
    });
    let mut fake_js_server_2 = fake_js_servers
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("restart all should start every stopped server")
        .unwrap();
    fake_js_server_2
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
}

#[gpui::test]
async fn test_configured_language_servers_projection(cx: &mut gpui::TestAppContext) {
    use project::lsp_store::{ConfiguredServerEntry, ConfiguredServerState};

    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir"), json!({ "a.rs": "", "b.js": "" }))
        .await;

    let project = Project::test(fs, [path!("/dir").as_ref()], cx).await;
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());

    let mut fake_rust_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "rust-lsp",
            ..Default::default()
        },
    );
    // An adapter without a fake server cannot resolve a binary, so it fails to start.
    language_registry.register_fake_lsp_adapter(
        "JavaScript",
        FakeLspAdapter {
            name: "broken-js-lsp",
            ..Default::default()
        },
    );
    language_registry.add(rust_lang());
    language_registry.add(js_lang());
    let worktree_id = project.read_with(cx, |project, cx| {
        project.worktrees(cx).next().unwrap().read(cx).id()
    });

    let tree_changes = std::rc::Rc::new(std::cell::Cell::new(0));
    cx.update({
        let tree_changes = tree_changes.clone();
        |cx| {
            cx.subscribe(&lsp_store, move |_, event, _| {
                if matches!(event, LspStoreEvent::ConfiguredLanguageServersChanged) {
                    tree_changes.set(tree_changes.get() + 1);
                }
            })
        }
    })
    .detach();

    let _rs_buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_1 = fake_rust_servers.next().await.unwrap();
    fake_rust_server_1
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    cx.executor().run_until_parked();
    assert_eq!(
        lsp_store.read_with(cx, |lsp_store, _| lsp_store.configured_language_servers()),
        vec![ConfiguredServerEntry {
            name: LanguageServerName::new_static("rust-lsp"),
            worktree_id,
            languages: vec![LanguageName::new("Rust")],
            state: ConfiguredServerState::Running(fake_rust_server_1.server.server_id()),
        }]
    );

    let _js_buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/b.js"), cx)
        })
        .await
        .unwrap();
    cx.executor().run_until_parked();
    let configured =
        lsp_store.read_with(cx, |lsp_store, _| lsp_store.configured_language_servers());
    assert_eq!(configured.len(), 2, "{configured:?}");
    assert_eq!(
        configured[0].name,
        LanguageServerName::new_static("broken-js-lsp")
    );
    assert_eq!(
        configured[0].languages,
        vec![LanguageName::new("JavaScript")]
    );
    match &configured[0].state {
        ConfiguredServerState::Failed(error) => {
            assert!(error.contains("disabled in tests"), "{error}")
        }
        other => panic!("expected the js server to have failed, got {other:?}"),
    }
    assert_eq!(
        configured[1].state,
        ConfiguredServerState::Running(fake_rust_server_1.server.server_id())
    );

    let tree_changes_before_stop = tree_changes.get();
    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.stop_all_language_servers(cx);
    });
    fake_rust_server_1
        .receive_notification::<lsp::notification::Exit>()
        .await;
    cx.executor().run_until_parked();
    assert!(
        tree_changes.get() > tree_changes_before_stop,
        "stopping must announce the configured servers change"
    );
    let configured =
        lsp_store.read_with(cx, |lsp_store, _| lsp_store.configured_language_servers());
    assert_eq!(
        configured
            .iter()
            .map(|server| (server.name.clone(), server.state.clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                LanguageServerName::new_static("broken-js-lsp"),
                ConfiguredServerState::Stopped
            ),
            (
                LanguageServerName::new_static("rust-lsp"),
                ConfiguredServerState::Stopped
            ),
        ]
    );

    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.restart_all_language_servers(cx);
    });
    let mut fake_rust_server_2 = fake_rust_servers
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("restart all should start the stopped rust server")
        .unwrap();
    fake_rust_server_2
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    cx.executor().run_until_parked();
    let configured =
        lsp_store.read_with(cx, |lsp_store, _| lsp_store.configured_language_servers());
    assert_eq!(
        configured[1].state,
        ConfiguredServerState::Running(fake_rust_server_2.server.server_id())
    );
    assert_matches!(configured[0].state, ConfiguredServerState::Failed(_));
}

struct SuspensionFixture {
    project: Entity<Project>,
    lsp_store: Entity<LspStore>,
    fake_rust_servers: futures::channel::mpsc::UnboundedReceiver<lsp::FakeLanguageServer>,
}

async fn suspension_fixture(
    fs: Arc<FakeFs>,
    root: &Path,
    cx: &mut gpui::TestAppContext,
) -> SuspensionFixture {
    let project = Project::test(fs, [root], cx).await;
    let lsp_store = project.read_with(cx, |project, _| project.lsp_store());
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    let fake_rust_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: "rust-lsp",
            ..Default::default()
        },
    );
    language_registry.add(rust_lang());
    SuspensionFixture {
        project,
        lsp_store,
        fake_rust_servers,
    }
}

fn set_suspension_settings(
    suspend_after_inactive_minutes: Option<u64>,
    max_active_projects: Option<usize>,
    suspend_over_budget_after_seconds: Option<u64>,
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        SettingsStore::update_global(cx, |settings, cx| {
            settings.update_user_settings(cx, |settings| {
                settings.global_lsp_settings = Some(GlobalLspSettingsContent {
                    suspend_after_inactive_minutes,
                    max_active_projects,
                    suspend_over_budget_after_seconds,
                    ..Default::default()
                });
            });
        })
    });
    cx.executor().run_until_parked();
}

fn configured_server_states(
    lsp_store: &Entity<LspStore>,
    cx: &mut gpui::TestAppContext,
) -> Vec<project::lsp_store::ConfiguredServerState> {
    lsp_store.read_with(cx, |lsp_store, _| {
        lsp_store
            .configured_language_servers()
            .into_iter()
            .map(|server| server.state)
            .collect()
    })
}

#[gpui::test]
async fn test_language_servers_suspend_after_inactivity(cx: &mut gpui::TestAppContext) {
    use project::lsp_store::ConfiguredServerState;

    init_test(cx);
    set_suspension_settings(Some(1), None, None, cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir"), json!({ "a.rs": "" })).await;
    let SuspensionFixture {
        project,
        lsp_store,
        mut fake_rust_servers,
    } = suspension_fixture(fs, path!("/dir").as_ref(), cx).await;

    let (_buffer, _handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_1 = fake_rust_servers.next().await.unwrap();
    fake_rust_server_1
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    project.update(cx, |project, cx| project.set_window_active(false, cx));
    cx.executor().advance_clock(Duration::from_secs(61));
    cx.executor().run_until_parked();
    fake_rust_server_1
        .receive_notification::<lsp::notification::Exit>()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("the language server should exit once the window has been idle long enough");
    cx.executor().run_until_parked();
    let states = configured_server_states(&lsp_store, cx);
    assert_eq!(states.len(), 1, "{states:?}");
    assert_matches!(states[0], ConfiguredServerState::Suspended { .. });

    project.update(cx, |project, cx| project.set_window_active(true, cx));
    let mut fake_rust_server_2 = fake_rust_servers
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("a language server should start again when the window becomes active")
        .unwrap();
    assert_eq!(
        fake_rust_server_2
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .await
            .text_document
            .uri
            .as_str(),
        uri!("file:///dir/a.rs")
    );
    cx.executor().run_until_parked();
    assert_eq!(
        configured_server_states(&lsp_store, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server_2.server.server_id()
        )]
    );
}

#[gpui::test]
async fn test_reactivation_cancels_pending_suspension(cx: &mut gpui::TestAppContext) {
    use project::lsp_store::ConfiguredServerState;

    init_test(cx);
    set_suspension_settings(Some(1), None, None, cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir"), json!({ "a.rs": "" })).await;
    let SuspensionFixture {
        project,
        lsp_store,
        mut fake_rust_servers,
    } = suspension_fixture(fs, path!("/dir").as_ref(), cx).await;

    let (_buffer, _handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server = fake_rust_servers.next().await.unwrap();
    fake_rust_server
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    project.update(cx, |project, cx| project.set_window_active(false, cx));
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.executor().run_until_parked();
    project.update(cx, |project, cx| project.set_window_active(true, cx));
    cx.executor().advance_clock(Duration::from_secs(120));
    cx.executor().run_until_parked();

    assert!(
        fake_rust_server
            .try_receive_notification::<lsp::notification::Exit>()
            .now_or_never()
            .flatten()
            .is_none(),
        "re-activating the window must cancel the pending suspension"
    );
    assert!(
        fake_rust_servers.next().now_or_never().flatten().is_none(),
        "no new language server should have been started"
    );
    assert_eq!(
        configured_server_states(&lsp_store, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server.server.server_id()
        )]
    );
}

#[gpui::test]
async fn test_idle_suspend_timer_survives_repeated_deactivation(cx: &mut gpui::TestAppContext) {
    use project::lsp_store::ConfiguredServerState;

    init_test(cx);
    set_suspension_settings(Some(10), None, None, cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir"), json!({ "a.rs": "" })).await;
    let SuspensionFixture {
        project,
        lsp_store,
        mut fake_rust_servers,
    } = suspension_fixture(fs, path!("/dir").as_ref(), cx).await;

    let (_buffer, _handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server = fake_rust_servers.next().await.unwrap();
    fake_rust_server
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    project.update(cx, |project, cx| project.set_window_active(false, cx));
    cx.executor().advance_clock(Duration::from_secs(5 * 60));
    cx.executor().run_until_parked();
    // Deactivating an already inactive window again must not postpone the suspension.
    project.update(cx, |project, cx| project.set_window_active(false, cx));
    cx.executor().advance_clock(Duration::from_secs(5 * 60));
    cx.executor().run_until_parked();

    fake_rust_server
        .receive_notification::<lsp::notification::Exit>()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("the language server should exit ten minutes after the first deactivation");
    cx.executor().run_until_parked();
    let states = configured_server_states(&lsp_store, cx);
    assert_eq!(states.len(), 1, "{states:?}");
    assert_matches!(states[0], ConfiguredServerState::Suspended { .. });
}

#[gpui::test]
async fn test_suspension_disabled_by_default(cx: &mut gpui::TestAppContext) {
    use project::lsp_store::ConfiguredServerState;

    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir"), json!({ "a.rs": "" })).await;
    let SuspensionFixture {
        project,
        lsp_store,
        mut fake_rust_servers,
    } = suspension_fixture(fs, path!("/dir").as_ref(), cx).await;

    let (_buffer, _handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server = fake_rust_servers.next().await.unwrap();
    fake_rust_server
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    project.update(cx, |project, cx| project.set_window_active(false, cx));
    cx.executor().advance_clock(Duration::from_secs(60 * 60));
    cx.executor().run_until_parked();

    assert!(
        fake_rust_server
            .try_receive_notification::<lsp::notification::Exit>()
            .now_or_never()
            .flatten()
            .is_none(),
        "language servers must keep running while suspension is not configured"
    );
    assert_eq!(
        configured_server_states(&lsp_store, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server.server.server_id()
        )]
    );
}

#[gpui::test]
async fn test_user_stopped_servers_stay_stopped_across_suspend_resume(
    cx: &mut gpui::TestAppContext,
) {
    use project::lsp_store::ConfiguredServerState;

    init_test(cx);
    set_suspension_settings(Some(1), None, None, cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir"), json!({ "a.rs": "" })).await;
    let SuspensionFixture {
        project,
        lsp_store,
        mut fake_rust_servers,
    } = suspension_fixture(fs, path!("/dir").as_ref(), cx).await;

    let (_buffer, _handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server = fake_rust_servers.next().await.unwrap();
    fake_rust_server
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.stop_all_language_servers(cx);
    });
    fake_rust_server
        .receive_notification::<lsp::notification::Exit>()
        .await;
    cx.executor().run_until_parked();
    assert_eq!(
        configured_server_states(&lsp_store, cx),
        vec![ConfiguredServerState::Stopped]
    );

    project.update(cx, |project, cx| project.set_window_active(false, cx));
    cx.executor().advance_clock(Duration::from_secs(61));
    cx.executor().run_until_parked();
    project.update(cx, |project, cx| project.set_window_active(true, cx));
    cx.executor().run_until_parked();

    assert!(
        fake_rust_servers.next().now_or_never().flatten().is_none(),
        "resuming from suspension must not revive servers the user stopped"
    );
    assert_eq!(
        configured_server_states(&lsp_store, cx),
        vec![ConfiguredServerState::Stopped]
    );
}

#[gpui::test]
async fn test_max_active_projects_budget(cx: &mut gpui::TestAppContext) {
    use project::lsp_store::ConfiguredServerState;

    init_test(cx);
    set_suspension_settings(None, Some(1), Some(0), cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir_a"), json!({ "a.rs": "" })).await;
    fs.insert_tree(path!("/dir_b"), json!({ "b.rs": "" })).await;
    let SuspensionFixture {
        project: project_a,
        lsp_store: lsp_store_a,
        fake_rust_servers: mut fake_rust_servers_a,
    } = suspension_fixture(fs.clone(), path!("/dir_a").as_ref(), cx).await;
    let SuspensionFixture {
        project: project_b,
        lsp_store: lsp_store_b,
        fake_rust_servers: mut fake_rust_servers_b,
    } = suspension_fixture(fs, path!("/dir_b").as_ref(), cx).await;

    project_a.update(cx, |project, cx| project.set_window_active(true, cx));
    let (_buffer_a, _handle_a) = project_a
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir_a/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_a1 = fake_rust_servers_a.next().await.unwrap();
    fake_rust_server_a1
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    // Opening a buffer starts project B's server; the budget is only enforced once a
    // window becomes active.
    let (_buffer_b, _handle_b) = project_b
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir_b/b.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_b1 = fake_rust_servers_b
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project B's server should start; without servers it must not have been suspended")
        .unwrap();
    assert_eq!(
        fake_rust_server_b1
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .await
            .text_document
            .uri
            .as_str(),
        uri!("file:///dir_b/b.rs")
    );

    // Activating a second project that runs a server exceeds the budget of one, so the
    // least recently active project gives up its servers.
    project_b.update(cx, |project, cx| project.set_window_active(true, cx));
    fake_rust_server_a1
        .receive_notification::<lsp::notification::Exit>()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project A's server should exit once project B becomes active");
    cx.executor().run_until_parked();
    let states_a = configured_server_states(&lsp_store_a, cx);
    assert_eq!(states_a.len(), 1, "{states_a:?}");
    assert_matches!(states_a[0], ConfiguredServerState::Suspended { .. });

    // Coming back to project A resumes it and suspends project B in turn.
    project_a.update(cx, |project, cx| project.set_window_active(true, cx));
    let mut fake_rust_server_a2 = fake_rust_servers_a
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project A's server should start again when it becomes active")
        .unwrap();
    assert_eq!(
        fake_rust_server_a2
            .receive_notification::<lsp::notification::DidOpenTextDocument>()
            .await
            .text_document
            .uri
            .as_str(),
        uri!("file:///dir_a/a.rs")
    );
    fake_rust_server_b1
        .receive_notification::<lsp::notification::Exit>()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project B's server should exit once project A becomes active again");
    cx.executor().run_until_parked();
    assert_eq!(
        configured_server_states(&lsp_store_a, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server_a2.server.server_id()
        )]
    );
    let states_b = configured_server_states(&lsp_store_b, cx);
    assert_eq!(states_b.len(), 1, "{states_b:?}");
    assert_matches!(states_b[0], ConfiguredServerState::Suspended { .. });
}

#[gpui::test]
async fn test_over_budget_grace_keeps_servers_when_reactivated(cx: &mut gpui::TestAppContext) {
    use project::lsp_store::ConfiguredServerState;

    init_test(cx);
    set_suspension_settings(None, Some(1), Some(30), cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir_a"), json!({ "a.rs": "" })).await;
    fs.insert_tree(path!("/dir_b"), json!({ "b.rs": "" })).await;
    let SuspensionFixture {
        project: project_a,
        lsp_store: lsp_store_a,
        fake_rust_servers: mut fake_rust_servers_a,
    } = suspension_fixture(fs.clone(), path!("/dir_a").as_ref(), cx).await;
    let SuspensionFixture {
        project: project_b,
        lsp_store: lsp_store_b,
        fake_rust_servers: mut fake_rust_servers_b,
    } = suspension_fixture(fs, path!("/dir_b").as_ref(), cx).await;

    project_a.update(cx, |project, cx| project.set_window_active(true, cx));
    let (_buffer_a, _handle_a) = project_a
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir_a/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_a = fake_rust_servers_a.next().await.unwrap();
    fake_rust_server_a
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    let (_buffer_b, _handle_b) = project_b
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir_b/b.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_b = fake_rust_servers_b
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project B's server should start")
        .unwrap();
    fake_rust_server_b
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    // Project A goes over budget but keeps its server for the grace period.
    project_b.update(cx, |project, cx| project.set_window_active(true, cx));
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.executor().run_until_parked();
    assert!(
        fake_rust_server_a
            .try_receive_notification::<lsp::notification::Exit>()
            .now_or_never()
            .flatten()
            .is_none(),
        "project A's server must keep running during the grace period"
    );
    assert_eq!(
        configured_server_states(&lsp_store_a, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server_a.server.server_id()
        )]
    );

    // Coming back within the grace period cancels A's pending suspension and puts B over
    // budget instead, with a grace period of its own.
    project_a.update(cx, |project, cx| project.set_window_active(true, cx));
    cx.executor().advance_clock(Duration::from_secs(25));
    cx.executor().run_until_parked();
    assert!(
        fake_rust_server_a
            .try_receive_notification::<lsp::notification::Exit>()
            .now_or_never()
            .flatten()
            .is_none(),
        "re-activating project A within the grace period must cancel its suspension"
    );
    assert!(
        fake_rust_servers_a
            .next()
            .now_or_never()
            .flatten()
            .is_none(),
        "project A's server must not have been restarted"
    );
    assert!(
        fake_rust_server_b
            .try_receive_notification::<lsp::notification::Exit>()
            .now_or_never()
            .flatten()
            .is_none(),
        "project B's server must keep running during B's own grace period"
    );
    assert_eq!(
        configured_server_states(&lsp_store_a, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server_a.server.server_id()
        )]
    );

    cx.executor().advance_clock(Duration::from_secs(10));
    cx.executor().run_until_parked();
    fake_rust_server_b
        .receive_notification::<lsp::notification::Exit>()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project B's server should exit once its grace period has elapsed");
    cx.executor().run_until_parked();
    let states_b = configured_server_states(&lsp_store_b, cx);
    assert_eq!(states_b.len(), 1, "{states_b:?}");
    assert_matches!(states_b[0], ConfiguredServerState::Suspended { .. });
    assert_eq!(
        configured_server_states(&lsp_store_a, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server_a.server.server_id()
        )]
    );
}

#[gpui::test]
async fn test_over_budget_grace_suspends_after_it_elapses(cx: &mut gpui::TestAppContext) {
    use project::lsp_store::ConfiguredServerState;

    init_test(cx);
    set_suspension_settings(None, Some(1), Some(30), cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir_a"), json!({ "a.rs": "" })).await;
    fs.insert_tree(path!("/dir_b"), json!({ "b.rs": "" })).await;
    let SuspensionFixture {
        project: project_a,
        lsp_store: lsp_store_a,
        fake_rust_servers: mut fake_rust_servers_a,
    } = suspension_fixture(fs.clone(), path!("/dir_a").as_ref(), cx).await;
    let SuspensionFixture {
        project: project_b,
        lsp_store: lsp_store_b,
        fake_rust_servers: mut fake_rust_servers_b,
    } = suspension_fixture(fs, path!("/dir_b").as_ref(), cx).await;

    project_a.update(cx, |project, cx| project.set_window_active(true, cx));
    let (_buffer_a, _handle_a) = project_a
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir_a/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_a = fake_rust_servers_a.next().await.unwrap();
    fake_rust_server_a
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    let (_buffer_b, _handle_b) = project_b
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir_b/b.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_b = fake_rust_servers_b
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project B's server should start")
        .unwrap();
    fake_rust_server_b
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    project_b.update(cx, |project, cx| project.set_window_active(true, cx));
    cx.executor().run_until_parked();
    assert!(
        fake_rust_server_a
            .try_receive_notification::<lsp::notification::Exit>()
            .now_or_never()
            .flatten()
            .is_none(),
        "project A's server must not exit before the grace period has elapsed"
    );

    cx.executor().advance_clock(Duration::from_secs(30));
    cx.executor().run_until_parked();
    fake_rust_server_a
        .receive_notification::<lsp::notification::Exit>()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project A's server should exit once the grace period has elapsed");
    cx.executor().run_until_parked();
    let states_a = configured_server_states(&lsp_store_a, cx);
    assert_eq!(states_a.len(), 1, "{states_a:?}");
    assert_matches!(states_a[0], ConfiguredServerState::Suspended { .. });
    assert_eq!(
        configured_server_states(&lsp_store_b, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server_b.server.server_id()
        )]
    );
}

#[gpui::test]
async fn test_projects_without_language_servers_take_no_budget_slot(cx: &mut gpui::TestAppContext) {
    use project::lsp_store::ConfiguredServerState;

    init_test(cx);
    set_suspension_settings(None, Some(1), Some(0), cx);

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir_a"), json!({ "a.rs": "" })).await;
    fs.insert_tree(path!("/dir_b"), json!({ "b.rs": "" })).await;
    fs.insert_tree(path!("/dir_c"), json!({ "c.txt": "" }))
        .await;
    let SuspensionFixture {
        project: project_a,
        lsp_store: lsp_store_a,
        fake_rust_servers: mut fake_rust_servers_a,
    } = suspension_fixture(fs.clone(), path!("/dir_a").as_ref(), cx).await;
    let SuspensionFixture {
        project: project_b,
        lsp_store: lsp_store_b,
        fake_rust_servers: mut fake_rust_servers_b,
    } = suspension_fixture(fs.clone(), path!("/dir_b").as_ref(), cx).await;
    let project_c = Project::test(fs, [path!("/dir_c").as_ref()], cx).await;

    project_a.update(cx, |project, cx| project.set_window_active(true, cx));
    let (_buffer_a, _handle_a) = project_a
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir_a/a.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_a = fake_rust_servers_a.next().await.unwrap();
    fake_rust_server_a
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;

    // A project that runs no language servers takes no slot of the budget.
    project_c.update(cx, |project, cx| project.set_window_active(true, cx));
    cx.executor().run_until_parked();
    assert!(
        fake_rust_server_a
            .try_receive_notification::<lsp::notification::Exit>()
            .now_or_never()
            .flatten()
            .is_none(),
        "activating a project without language servers must not suspend project A"
    );
    assert_eq!(
        configured_server_states(&lsp_store_a, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server_a.server.server_id()
        )]
    );

    // A project whose server is running does take a slot.
    let (_buffer_b, _handle_b) = project_b
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(path!("/dir_b/b.rs"), cx)
        })
        .await
        .unwrap();
    let mut fake_rust_server_b = fake_rust_servers_b
        .next()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project B's server should start; without servers it must not have been suspended")
        .unwrap();
    fake_rust_server_b
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    project_b.update(cx, |project, cx| project.set_window_active(true, cx));
    fake_rust_server_a
        .receive_notification::<lsp::notification::Exit>()
        .with_timeout(Duration::from_secs(5), &cx.executor())
        .await
        .expect("project A's server should exit once project B, which runs a server, is active");
    cx.executor().run_until_parked();
    let states_a = configured_server_states(&lsp_store_a, cx);
    assert_eq!(states_a.len(), 1, "{states_a:?}");
    assert_matches!(states_a[0], ConfiguredServerState::Suspended { .. });
    assert_eq!(
        configured_server_states(&lsp_store_b, cx),
        vec![ConfiguredServerState::Running(
            fake_rust_server_b.server.server_id()
        )]
    );
}
