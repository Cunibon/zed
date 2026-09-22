use std::{path::Path, time::Duration};

use collections::{BTreeSet, HashSet};
use fs::FakeFs;
use futures::{FutureExt as _, StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui::{Entity, FutureExt as _, UpdateGlobal as _};
use language::{Buffer, FakeLspAdapter, LanguageName, rust_lang};
use lsp::{FakeLanguageServer, LanguageServerName};
use pretty_assertions::{assert_eq, assert_matches};
use project::{
    LspStore, LspStoreEvent, Project,
    lsp_store::{ConfiguredServerEntry, ConfiguredServerState, OpenLspBufferHandle},
};
use serde_json::json;
use settings::{GlobalLspSettingsContent, SettingsStore};
use util::{path, uri};

use crate::{init_test, js_lang};

/// How long to wait for a fake language server to start or exit before failing the test.
const FAKE_SERVER_TIMEOUT: Duration = Duration::from_secs(5);

const GRACE_SECS: u64 = 30;
const GRACE: Duration = Duration::from_secs(GRACE_SECS);

async fn expect_server_start(
    servers: &mut UnboundedReceiver<FakeLanguageServer>,
    message: &str,
    cx: &mut gpui::TestAppContext,
) -> FakeLanguageServer {
    servers
        .next()
        .with_timeout(FAKE_SERVER_TIMEOUT, &cx.executor())
        .await
        .ok()
        .flatten()
        .expect(message)
}

/// Waits for `server` to exit and lets the store finish processing the shutdown.
async fn expect_exit(
    server: &mut FakeLanguageServer,
    message: &str,
    cx: &mut gpui::TestAppContext,
) {
    server
        .receive_notification::<lsp::notification::Exit>()
        .with_timeout(FAKE_SERVER_TIMEOUT, &cx.executor())
        .await
        .expect(message);
    cx.executor().run_until_parked();
}

async fn expect_did_open(server: &mut FakeLanguageServer, abs_path: &Path) {
    let opened = server
        .receive_notification::<lsp::notification::DidOpenTextDocument>()
        .await;
    assert_eq!(
        opened.text_document.uri,
        lsp::Uri::from_file_path(abs_path).expect("test paths are absolute")
    );
}

fn assert_no_server_started(servers: &mut UnboundedReceiver<FakeLanguageServer>, message: &str) {
    assert!(
        servers.next().now_or_never().flatten().is_none(),
        "{message}"
    );
}

/// Asserts that `server` has not been asked to exit.
fn assert_still_running(server: &mut FakeLanguageServer, message: &str) {
    assert!(
        server
            .try_receive_notification::<lsp::notification::Exit>()
            .now_or_never()
            .flatten()
            .is_none(),
        "{message}"
    );
}

pub(crate) fn configured_server_states(
    lsp_store: &Entity<LspStore>,
    cx: &mut gpui::TestAppContext,
) -> Vec<ConfiguredServerState> {
    lsp_store.read_with(cx, |lsp_store, _| {
        lsp_store
            .configured_language_servers()
            .into_iter()
            .map(|server| server.state)
            .collect()
    })
}

/// Asserts that `server` is the only configured server of the project and is running.
fn assert_running(
    lsp_store: &Entity<LspStore>,
    server: &FakeLanguageServer,
    cx: &mut gpui::TestAppContext,
) {
    assert_eq!(
        configured_server_states(lsp_store, cx),
        vec![ConfiguredServerState::Running(server.server.server_id())]
    );
}

/// Asserts that the project's only configured server is suspended.
fn assert_suspended(lsp_store: &Entity<LspStore>, cx: &mut gpui::TestAppContext) {
    let states = configured_server_states(lsp_store, cx);
    assert_matches!(
        states.as_slice(),
        [ConfiguredServerState::Suspended { .. }],
        "{states:?}"
    );
}

/// Moves the test clock forward and runs everything that became due.
fn advance(cx: &mut gpui::TestAppContext, duration: Duration) {
    cx.executor().advance_clock(duration);
    cx.executor().run_until_parked();
}

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
    let mut fake_rust_server_1 = expect_server_start(
        &mut fake_rust_servers,
        "opening a.rs should start the rust language server",
        cx,
    )
    .await;
    expect_did_open(&mut fake_rust_server_1, path!("/dir/a.rs").as_ref()).await;

    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.stop_all_language_servers(cx);
    });
    expect_exit(
        &mut fake_rust_server_1,
        "stopping all servers should shut the rust language server down",
        cx,
    )
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
    assert_no_server_started(
        &mut fake_rust_servers,
        "no language server should start while all servers are stopped",
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
    let mut fake_rust_server_2 = expect_server_start(
        &mut fake_rust_servers,
        "a language server should start after restarting with clear_stopped",
        cx,
    )
    .await;
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
    expect_exit(
        &mut fake_rust_server_2,
        "stopping all servers again should shut the rust language server down",
        cx,
    )
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
    let mut fake_rust_server_3 = expect_server_start(
        &mut fake_rust_servers,
        "a language server should start after start_language_server_for",
        cx,
    )
    .await;
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
    let mut fake_rust_server_1 = expect_server_start(
        &mut fake_rust_servers,
        "opening a.rs should start the rust language server",
        cx,
    )
    .await;
    expect_did_open(&mut fake_rust_server_1, path!("/dir/a.rs").as_ref()).await;
    let mut fake_js_server_1 = expect_server_start(
        &mut fake_js_servers,
        "opening b.js should start the js language server",
        cx,
    )
    .await;
    expect_did_open(&mut fake_js_server_1, path!("/dir/b.js").as_ref()).await;

    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.stop_all_language_servers(cx);
    });
    expect_exit(
        &mut fake_rust_server_1,
        "stopping all servers should shut the rust language server down",
        cx,
    )
    .await;
    expect_exit(
        &mut fake_js_server_1,
        "stopping all servers should shut the js language server down",
        cx,
    )
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
    let mut fake_rust_server_2 = expect_server_start(
        &mut fake_rust_servers,
        "the started server should come up",
        cx,
    )
    .await;
    expect_did_open(&mut fake_rust_server_2, path!("/dir/a.rs").as_ref()).await;
    cx.executor().run_until_parked();
    assert_no_server_started(
        &mut fake_js_servers,
        "starting one server must not revive the other stopped server",
    );

    lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.restart_all_language_servers(cx);
    });
    let mut fake_js_server_2 = expect_server_start(
        &mut fake_js_servers,
        "restart all should start every stopped server",
        cx,
    )
    .await;
    expect_did_open(&mut fake_js_server_2, path!("/dir/b.js").as_ref()).await;
}

#[gpui::test]
async fn test_configured_language_servers_projection(cx: &mut gpui::TestAppContext) {
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
    let mut fake_rust_server_1 = expect_server_start(
        &mut fake_rust_servers,
        "opening a.rs should start the rust language server",
        cx,
    )
    .await;
    expect_did_open(&mut fake_rust_server_1, path!("/dir/a.rs").as_ref()).await;
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
    expect_exit(
        &mut fake_rust_server_1,
        "stopping all servers should shut the rust language server down",
        cx,
    )
    .await;
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
    let mut fake_rust_server_2 = expect_server_start(
        &mut fake_rust_servers,
        "restart all should start the stopped rust server",
        cx,
    )
    .await;
    expect_did_open(&mut fake_rust_server_2, path!("/dir/a.rs").as_ref()).await;
    cx.executor().run_until_parked();
    let configured =
        lsp_store.read_with(cx, |lsp_store, _| lsp_store.configured_language_servers());
    assert_eq!(
        configured[1].state,
        ConfiguredServerState::Running(fake_rust_server_2.server.server_id())
    );
    assert_matches!(configured[0].state, ConfiguredServerState::Failed(_));
}

/// A project with `a.rs` open and the fake rust server that opened it running.
struct SuspensionFixture {
    project: Entity<Project>,
    lsp_store: Entity<LspStore>,
    fake_rust_servers: UnboundedReceiver<FakeLanguageServer>,
    fake_rust_server: FakeLanguageServer,
    _buffer: Entity<Buffer>,
    _lsp_handle: OpenLspBufferHandle,
}

async fn suspension_fixture(root: &Path, cx: &mut gpui::TestAppContext) -> SuspensionFixture {
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(root, json!({ "a.rs": "" })).await;
    let project = Project::test(fs, [root], cx).await;
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

    let file_path = root.join("a.rs");
    let (buffer, lsp_handle) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp(&file_path, cx)
        })
        .await
        .expect("the fixture's worktree contains a.rs");
    let mut fake_rust_server = expect_server_start(
        &mut fake_rust_servers,
        "opening a.rs should start the project's language server",
        cx,
    )
    .await;
    expect_did_open(&mut fake_rust_server, &file_path).await;

    SuspensionFixture {
        project,
        lsp_store,
        fake_rust_servers,
        fake_rust_server,
        _buffer: buffer,
        _lsp_handle: lsp_handle,
    }
}

/// Projects A and B, each running a server, with A's window activated before B was opened.
/// The budget is enforced only when a window becomes active, so both servers keep running
/// until B is activated in turn.
async fn two_project_budget_fixture(
    cx: &mut gpui::TestAppContext,
) -> (SuspensionFixture, SuspensionFixture) {
    let project_a = suspension_fixture(path!("/dir_a").as_ref(), cx).await;
    project_a
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    let project_b = suspension_fixture(path!("/dir_b").as_ref(), cx).await;
    (project_a, project_b)
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

#[gpui::test]
async fn test_language_servers_suspend_after_inactivity(cx: &mut gpui::TestAppContext) {
    const IDLE_MINUTES: u64 = 1;
    const IDLE: Duration = Duration::from_secs(IDLE_MINUTES * 60);

    init_test(cx);
    set_suspension_settings(Some(IDLE_MINUTES), None, None, cx);
    let mut fixture = suspension_fixture(path!("/dir").as_ref(), cx).await;

    fixture
        .project
        .update(cx, |project, cx| project.set_window_active(false, cx));
    advance(cx, IDLE + Duration::from_secs(1));
    expect_exit(
        &mut fixture.fake_rust_server,
        "the language server should exit once the window has been idle long enough",
        cx,
    )
    .await;
    assert_suspended(&fixture.lsp_store, cx);

    fixture
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    let mut fake_rust_server_2 = expect_server_start(
        &mut fixture.fake_rust_servers,
        "a language server should start again when the window becomes active",
        cx,
    )
    .await;
    expect_did_open(&mut fake_rust_server_2, path!("/dir/a.rs").as_ref()).await;
    cx.executor().run_until_parked();
    assert_running(&fixture.lsp_store, &fake_rust_server_2, cx);
}

#[gpui::test]
async fn test_reactivation_cancels_pending_suspension(cx: &mut gpui::TestAppContext) {
    const IDLE_MINUTES: u64 = 1;
    const IDLE: Duration = Duration::from_secs(IDLE_MINUTES * 60);

    init_test(cx);
    set_suspension_settings(Some(IDLE_MINUTES), None, None, cx);
    let mut fixture = suspension_fixture(path!("/dir").as_ref(), cx).await;

    fixture
        .project
        .update(cx, |project, cx| project.set_window_active(false, cx));
    advance(cx, IDLE / 2);
    fixture
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    advance(cx, IDLE * 2);

    assert_still_running(
        &mut fixture.fake_rust_server,
        "re-activating the window must cancel the pending suspension",
    );
    assert_no_server_started(
        &mut fixture.fake_rust_servers,
        "no new language server should have been started",
    );
    assert_running(&fixture.lsp_store, &fixture.fake_rust_server, cx);
}

#[gpui::test]
async fn test_idle_suspend_timer_survives_repeated_deactivation(cx: &mut gpui::TestAppContext) {
    const IDLE_MINUTES: u64 = 10;
    const IDLE: Duration = Duration::from_secs(IDLE_MINUTES * 60);

    init_test(cx);
    set_suspension_settings(Some(IDLE_MINUTES), None, None, cx);
    let mut fixture = suspension_fixture(path!("/dir").as_ref(), cx).await;

    fixture
        .project
        .update(cx, |project, cx| project.set_window_active(false, cx));
    advance(cx, IDLE / 2);
    // Deactivating an already inactive window again must not postpone the suspension.
    fixture
        .project
        .update(cx, |project, cx| project.set_window_active(false, cx));
    advance(cx, IDLE / 2);

    expect_exit(
        &mut fixture.fake_rust_server,
        "the language server should exit once the idle delay has passed since the first deactivation",
        cx,
    )
    .await;
    assert_suspended(&fixture.lsp_store, cx);
}

#[gpui::test]
async fn test_suspension_disabled_by_default(cx: &mut gpui::TestAppContext) {
    init_test(cx);
    let mut fixture = suspension_fixture(path!("/dir").as_ref(), cx).await;

    fixture
        .project
        .update(cx, |project, cx| project.set_window_active(false, cx));
    advance(cx, Duration::from_secs(60 * 60));

    assert_still_running(
        &mut fixture.fake_rust_server,
        "language servers must keep running while suspension is not configured",
    );
    assert_running(&fixture.lsp_store, &fixture.fake_rust_server, cx);
}

#[gpui::test]
async fn test_user_stopped_servers_stay_stopped_across_suspend_resume(
    cx: &mut gpui::TestAppContext,
) {
    const IDLE_MINUTES: u64 = 1;
    const IDLE: Duration = Duration::from_secs(IDLE_MINUTES * 60);

    init_test(cx);
    set_suspension_settings(Some(IDLE_MINUTES), None, None, cx);
    let mut fixture = suspension_fixture(path!("/dir").as_ref(), cx).await;

    fixture.lsp_store.update(cx, |lsp_store, cx| {
        lsp_store.stop_all_language_servers(cx);
    });
    expect_exit(
        &mut fixture.fake_rust_server,
        "stopping all servers should shut the language server down",
        cx,
    )
    .await;
    assert_eq!(
        configured_server_states(&fixture.lsp_store, cx),
        vec![ConfiguredServerState::Stopped]
    );

    fixture
        .project
        .update(cx, |project, cx| project.set_window_active(false, cx));
    advance(cx, IDLE + Duration::from_secs(1));
    fixture
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    cx.executor().run_until_parked();

    assert_no_server_started(
        &mut fixture.fake_rust_servers,
        "resuming from suspension must not revive servers the user stopped",
    );
    assert_eq!(
        configured_server_states(&fixture.lsp_store, cx),
        vec![ConfiguredServerState::Stopped]
    );
}

#[gpui::test]
async fn test_max_active_projects_budget(cx: &mut gpui::TestAppContext) {
    init_test(cx);
    set_suspension_settings(None, Some(1), Some(0), cx);
    let (mut project_a, mut project_b) = two_project_budget_fixture(cx).await;

    // Activating a second project that runs a server exceeds the budget of one, so the
    // least recently active project gives up its servers.
    project_b
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    expect_exit(
        &mut project_a.fake_rust_server,
        "project A's server should exit once project B becomes active",
        cx,
    )
    .await;
    assert_suspended(&project_a.lsp_store, cx);

    // Coming back to project A resumes it and suspends project B in turn.
    project_a
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    let mut fake_rust_server_a2 = expect_server_start(
        &mut project_a.fake_rust_servers,
        "project A's server should start again when it becomes active",
        cx,
    )
    .await;
    expect_did_open(&mut fake_rust_server_a2, path!("/dir_a/a.rs").as_ref()).await;
    expect_exit(
        &mut project_b.fake_rust_server,
        "project B's server should exit once project A becomes active again",
        cx,
    )
    .await;
    assert_running(&project_a.lsp_store, &fake_rust_server_a2, cx);
    assert_suspended(&project_b.lsp_store, cx);
}

#[gpui::test]
async fn test_over_budget_grace_keeps_servers_when_reactivated(cx: &mut gpui::TestAppContext) {
    init_test(cx);
    set_suspension_settings(None, Some(1), Some(GRACE_SECS), cx);
    let (mut project_a, mut project_b) = two_project_budget_fixture(cx).await;
    let step = GRACE / 3;

    // Project A goes over budget but keeps its server for the grace period.
    project_b
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    advance(cx, step);
    assert_still_running(
        &mut project_a.fake_rust_server,
        "project A's server must keep running during the grace period",
    );
    assert_running(&project_a.lsp_store, &project_a.fake_rust_server, cx);

    // Coming back within the grace period cancels A's pending suspension and puts B over
    // budget instead, with a grace period of its own. A's cancelled deadline comes up one
    // step before B's.
    project_a
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    advance(cx, GRACE - step);
    assert_still_running(
        &mut project_a.fake_rust_server,
        "re-activating project A within the grace period must cancel its suspension",
    );
    assert_no_server_started(
        &mut project_a.fake_rust_servers,
        "project A's server must not have been restarted",
    );
    assert_still_running(
        &mut project_b.fake_rust_server,
        "project B's server must keep running during B's own grace period",
    );
    assert_running(&project_a.lsp_store, &project_a.fake_rust_server, cx);

    advance(cx, step);
    expect_exit(
        &mut project_b.fake_rust_server,
        "project B's server should exit once its grace period has elapsed",
        cx,
    )
    .await;
    assert_suspended(&project_b.lsp_store, cx);
    assert_running(&project_a.lsp_store, &project_a.fake_rust_server, cx);
}

#[gpui::test]
async fn test_over_budget_grace_suspends_after_it_elapses(cx: &mut gpui::TestAppContext) {
    init_test(cx);
    set_suspension_settings(None, Some(1), Some(GRACE_SECS), cx);
    let (mut project_a, project_b) = two_project_budget_fixture(cx).await;

    project_b
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    cx.executor().run_until_parked();
    assert_still_running(
        &mut project_a.fake_rust_server,
        "project A's server must not exit before the grace period has elapsed",
    );

    advance(cx, GRACE);
    expect_exit(
        &mut project_a.fake_rust_server,
        "project A's server should exit once the grace period has elapsed",
        cx,
    )
    .await;
    assert_suspended(&project_a.lsp_store, cx);
    assert_running(&project_b.lsp_store, &project_b.fake_rust_server, cx);
}

#[gpui::test]
async fn test_projects_without_language_servers_take_no_budget_slot(cx: &mut gpui::TestAppContext) {
    init_test(cx);
    set_suspension_settings(None, Some(1), Some(0), cx);
    let mut project_a = suspension_fixture(path!("/dir_a").as_ref(), cx).await;
    project_a
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(path!("/dir_c"), json!({ "c.txt": "" }))
        .await;
    let project_c = Project::test(fs, [path!("/dir_c").as_ref()], cx).await;

    // A project that runs no language servers takes no slot of the budget.
    project_c.update(cx, |project, cx| project.set_window_active(true, cx));
    cx.executor().run_until_parked();
    assert_still_running(
        &mut project_a.fake_rust_server,
        "activating a project without language servers must not suspend project A",
    );
    assert_running(&project_a.lsp_store, &project_a.fake_rust_server, cx);

    // A project whose server is running does take a slot.
    let project_b = suspension_fixture(path!("/dir_b").as_ref(), cx).await;
    project_b
        .project
        .update(cx, |project, cx| project.set_window_active(true, cx));
    expect_exit(
        &mut project_a.fake_rust_server,
        "project A's server should exit once project B, which runs a server, is active",
        cx,
    )
    .await;
    assert_suspended(&project_a.lsp_store, cx);
    assert_running(&project_b.lsp_store, &project_b.fake_rust_server, cx);
}
