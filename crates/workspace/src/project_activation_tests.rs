use super::*;
use crate::tests::init_test;
use fs::FakeFs;
use gpui::TestAppContext;

/// Opens a workspace window on a shared `AppState`, the way the real application does,
/// so that windows share the `WorkspaceStore` that arbitrates project activation.
fn add_workspace_window(
    project: &Entity<Project>,
    app_state: &Arc<AppState>,
    cx: &mut TestAppContext,
) -> WindowHandle<Workspace> {
    let project = project.clone();
    let app_state = app_state.clone();
    cx.add_window(|window, cx| Workspace::new(Default::default(), project, app_state, window, cx))
}

#[gpui::test]
async fn test_long_activation_burst_activates_only_the_final_key_window(
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx);

    let app_state = cx.update(|cx| AppState::test(cx));
    let fs = FakeFs::new(cx.executor());
    let project_a = Project::test(fs.clone(), [], cx).await;
    let project_b = Project::test(fs.clone(), [], cx).await;
    let project_c = Project::test(fs, [], cx).await;
    let window_a = add_workspace_window(&project_a, &app_state, cx);
    let window_b = add_workspace_window(&project_b, &app_state, cx);
    let window_c = add_workspace_window(&project_c, &app_state, cx);
    cx.run_until_parked();

    // Windows B and C sit in the background. The user last worked in window A before
    // leaving the application, so A remains the platform's active window while every
    // project is inactive.
    cx.simulate_window_active_status_change(window_b.into(), false);
    cx.simulate_window_active_status_change(window_c.into(), false);
    window_a
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(!project_b.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(!project_c.read_with(cx, |project, cx| project.is_window_active(cx)));
    cx.simulate_window_active_status_change(window_a.into(), false);
    cx.run_until_parked();
    assert!(!project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert_eq!(cx.update(|cx| cx.active_window()), Some(window_a.into()));

    // When the application regains focus, AppKit makes A, then B, then C key. Each key
    // change renders a frame synchronously, so on a stalled main thread the gaps between
    // the callbacks are longer than any per-window settle delay.
    cx.simulate_window_active_status_change(window_a.into(), true);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert!(
        !project_a.read_with(cx, |project, cx| project.is_window_active(cx)),
        "a window re-keyed early in a long burst must not activate its project"
    );
    assert!(!project_b.read_with(cx, |project, cx| project.is_window_active(cx)));

    cx.simulate_window_active_status_change(window_b.into(), true);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert!(!project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(
        !project_b.read_with(cx, |project, cx| project.is_window_active(cx)),
        "a window re-keyed in the middle of a long burst must not activate its project"
    );

    window_c
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    assert!(!project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(!project_b.read_with(cx, |project, cx| project.is_window_active(cx)));
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(!project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(!project_b.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(
        project_c.read_with(cx, |project, cx| project.is_window_active(cx)),
        "only the window that is active once the burst has ended may activate its project"
    );
}

#[gpui::test]
async fn test_spurious_activation_of_background_window_does_not_activate_its_project(
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx);

    let app_state = cx.update(|cx| AppState::test(cx));
    let fs = FakeFs::new(cx.executor());
    let project_a = Project::test(fs.clone(), [], cx).await;
    let project_b = Project::test(fs, [], cx).await;
    let window_a = add_workspace_window(&project_a, &app_state, cx);
    let window_b = add_workspace_window(&project_b, &app_state, cx);
    cx.run_until_parked();
    window_b
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.active_window()), Some(window_b.into()));
    assert!(!project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(project_b.read_with(cx, |project, cx| project.is_window_active(cx)));

    // macOS reports every window as active when the application regains focus, so
    // window A hears that it is active while window B remains the key window.
    cx.simulate_window_active_status_change(window_a.into(), true);
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| window_a.is_active(cx)), Some(true));
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(
        !project_a.read_with(cx, |project, cx| project.is_window_active(cx)),
        "a window that is not the platform's active window must not activate its project"
    );
    assert!(
        project_b.read_with(cx, |project, cx| project.is_window_active(cx)),
        "the platform's active window must keep its project active"
    );
}

#[gpui::test]
async fn test_window_deactivation_reaches_project_immediately(cx: &mut gpui::TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    let project = Project::test(fs, [], cx).await;
    let (_workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(project.read_with(cx, |project, cx| project.is_window_active(cx)));

    cx.deactivate_window();
    assert!(
        !project.read_with(cx, |project, cx| project.is_window_active(cx)),
        "deactivation must reach the project without waiting for the settle delay"
    );

    cx.update(|window, _| window.activate_window());
    cx.executor().run_until_parked();
    assert!(!project.read_with(cx, |project, cx| project.is_window_active(cx)));
    cx.deactivate_window();
    assert!(!project.read_with(cx, |project, cx| project.is_window_active(cx)));
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(
        !project.read_with(cx, |project, cx| project.is_window_active(cx)),
        "a deactivation during the settle delay must leave the project inactive"
    );
}

#[gpui::test]
async fn test_rapid_window_activations_coalesce(cx: &mut gpui::TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    let project = Project::test(fs, [], cx).await;
    let (_workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
    cx.deactivate_window();
    assert!(!project.read_with(cx, |project, cx| project.is_window_active(cx)));

    cx.update(|window, _| window.activate_window());
    cx.executor().run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert!(!project.read_with(cx, |project, cx| project.is_window_active(cx)));

    // A second activation of the same window arrives while the first is still pending.
    cx.simulate_active_status_change(true);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert!(
        !project.read_with(cx, |project, cx| project.is_window_active(cx)),
        "a repeated activation must restart the settle delay rather than let the first fire"
    );

    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert!(
        project.read_with(cx, |project, cx| project.is_window_active(cx)),
        "the coalesced activation must reach the project once the delay has elapsed"
    );
}

#[gpui::test]
async fn test_window_closed_during_activation_burst_keeps_decision_for_the_others(
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx);

    let app_state = cx.update(|cx| AppState::test(cx));
    let fs = FakeFs::new(cx.executor());
    let project_a = Project::test(fs.clone(), [], cx).await;
    let project_b = Project::test(fs, [], cx).await;
    let window_a = add_workspace_window(&project_a, &app_state, cx);
    let window_b = add_workspace_window(&project_b, &app_state, cx);
    cx.run_until_parked();
    window_b
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(!project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(project_b.read_with(cx, |project, cx| project.is_window_active(cx)));

    window_a
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    assert!(!project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(!project_b.read_with(cx, |project, cx| project.is_window_active(cx)));

    // Window B closes while the decision for the burst is still pending.
    window_b
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| window_b.is_active(cx)), None);

    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(
        project_a.read_with(cx, |project, cx| project.is_window_active(cx)),
        "the remaining window must still get its decision"
    );
    assert!(!project_b.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert_eq!(
        app_state
            .workspace_store
            .read_with(cx, |store, _| store.workspaces().count()),
        1,
        "the closed workspace must be dropped from the store"
    );
}

/// Opens a window that holds two workspaces on a shared `AppState`; the first one is the
/// workspace the window displays.
fn add_multi_workspace_window(
    displayed_project: &Entity<Project>,
    held_project: &Entity<Project>,
    app_state: &Arc<AppState>,
    cx: &mut TestAppContext,
) -> (WindowHandle<MultiWorkspace>, Entity<Workspace>) {
    let window = cx.add_window({
        let project = displayed_project.clone();
        let app_state = app_state.clone();
        |window, cx| {
            let workspace =
                cx.new(|cx| Workspace::new(Default::default(), project, app_state, window, cx));
            MultiWorkspace::new(workspace, window, cx)
        }
    });
    let held_workspace = window
        .update(cx, {
            let project = held_project.clone();
            let app_state = app_state.clone();
            |multi_workspace, window, cx| {
                let workspace =
                    cx.new(|cx| Workspace::new(Default::default(), project, app_state, window, cx));
                multi_workspace.add(workspace.clone(), window, cx);
                workspace
            }
        })
        .unwrap();
    (window, held_workspace)
}

#[gpui::test]
async fn test_window_activation_activates_only_the_workspace_the_window_displays(
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx);

    let app_state = cx.update(|cx| AppState::test(cx));
    let fs = FakeFs::new(cx.executor());
    let project_a = Project::test(fs.clone(), [], cx).await;
    let project_b = Project::test(fs, [], cx).await;
    let (window, _workspace_b) = add_multi_workspace_window(&project_a, &project_b, &app_state, cx);
    cx.run_until_parked();
    window
        .read_with(cx, |multi_workspace, _| {
            assert_eq!(multi_workspace.workspaces().count(), 2);
        })
        .unwrap();

    window
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(
        project_a.read_with(cx, |project, cx| project.is_window_active(cx)),
        "the project of the workspace the window displays must activate"
    );
    assert!(
        !project_b.read_with(cx, |project, cx| project.is_window_active(cx)),
        "a workspace the window holds in the background shares the window handle but must not activate its project"
    );
}

#[gpui::test]
async fn test_switching_the_displayed_workspace_moves_project_activation(
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx);

    let app_state = cx.update(|cx| AppState::test(cx));
    let fs = FakeFs::new(cx.executor());
    let project_a = Project::test(fs.clone(), [], cx).await;
    let project_b = Project::test(fs, [], cx).await;
    let (window, workspace_b) = add_multi_workspace_window(&project_a, &project_b, &app_state, cx);
    cx.run_until_parked();
    window
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(!project_b.read_with(cx, |project, cx| project.is_window_active(cx)));

    // The user picks workspace B in the sidebar; the window stays the platform's active one.
    window
        .update(cx, |multi_workspace, window, cx| {
            multi_workspace.activate(workspace_b.clone(), None, window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .read_with(cx, |multi_workspace, _| {
            assert!(multi_workspace.workspace() == &workspace_b);
        })
        .unwrap();
    assert!(
        !project_b.read_with(cx, |project, cx| project.is_window_active(cx)),
        "a workspace switch must wait for the settle delay like a window activation"
    );

    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(
        !project_a.read_with(cx, |project, cx| project.is_window_active(cx)),
        "the workspace that left the front of the window must deactivate its project"
    );
    assert!(
        project_b.read_with(cx, |project, cx| project.is_window_active(cx)),
        "the workspace brought to the front of the window must activate its project"
    );
}

#[gpui::test]
async fn test_window_deactivation_reaches_the_projects_of_held_workspaces(
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx);

    let app_state = cx.update(|cx| AppState::test(cx));
    let fs = FakeFs::new(cx.executor());
    let project_a = Project::test(fs.clone(), [], cx).await;
    let project_b = Project::test(fs, [], cx).await;
    let (window, workspace_b) = add_multi_workspace_window(&project_a, &project_b, &app_state, cx);
    cx.run_until_parked();
    window
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert!(project_a.read_with(cx, |project, cx| project.is_window_active(cx)));
    assert!(!project_b.read_with(cx, |project, cx| project.is_window_active(cx)));

    // Workspace B moves to the front before the switch has settled, so the window now
    // holds workspace A in the background while A's project is still active.
    window
        .update(cx, |multi_workspace, window, cx| {
            multi_workspace.activate(workspace_b.clone(), None, window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .read_with(cx, |multi_workspace, _| {
            assert_eq!(multi_workspace.workspaces().count(), 2);
            assert!(multi_workspace.workspace() == &workspace_b);
        })
        .unwrap();
    assert!(project_a.read_with(cx, |project, cx| project.is_window_active(cx)));

    cx.simulate_window_active_status_change(window.into(), false);
    cx.run_until_parked();
    assert!(
        !project_a.read_with(cx, |project, cx| project.is_window_active(cx)),
        "deactivation must reach a workspace the window holds in the background without waiting for the settle delay"
    );
    assert!(
        !project_b.read_with(cx, |project, cx| project.is_window_active(cx)),
        "deactivation must reach the workspace the window displays without waiting for the settle delay"
    );
}
