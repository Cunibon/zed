use super::*;
use crate::tests::init_test;
use fs::FakeFs;
use gpui::TestAppContext;

/// Lets the pending decision fire: advances past the settle delay and drains the executor.
fn settle(cx: &mut TestAppContext) {
    cx.executor()
        .advance_clock(PROJECT_ACTIVATION_SETTLE_DELAY + Duration::from_millis(50));
    cx.run_until_parked();
}

/// Advances half of what `settle` does, so that a single call leaves the decision pending
/// while two consecutive calls let it fire.
fn half_settle(cx: &mut TestAppContext) {
    cx.executor()
        .advance_clock((PROJECT_ACTIVATION_SETTLE_DELAY + Duration::from_millis(50)) / 2);
    cx.run_until_parked();
}

fn is_active(project: &Entity<Project>, cx: &TestAppContext) -> bool {
    project.read_with(cx, |project, cx| project.is_window_active(cx))
}

/// Makes `window` the platform's active window and lets its activation callback run.
fn activate<V: 'static>(window: &WindowHandle<V>, cx: &mut TestAppContext) {
    window
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
}

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

/// Two windows, each displaying its own project, on a shared `AppState`.
async fn two_workspace_windows(
    cx: &mut TestAppContext,
) -> (
    Arc<AppState>,
    (Entity<Project>, WindowHandle<Workspace>),
    (Entity<Project>, WindowHandle<Workspace>),
) {
    let app_state = cx.update(|cx| AppState::test(cx));
    let fs = FakeFs::new(cx.executor());
    let project_a = Project::test(fs.clone(), [], cx).await;
    let project_b = Project::test(fs, [], cx).await;
    let window_a = add_workspace_window(&project_a, &app_state, cx);
    let window_b = add_workspace_window(&project_b, &app_state, cx);
    cx.run_until_parked();
    (app_state, (project_a, window_a), (project_b, window_b))
}

/// One window holding two workspaces on a shared `AppState`: it displays the first project's
/// workspace and holds the second project's workspace, which is returned, in the background.
async fn multi_workspace_window(
    cx: &mut TestAppContext,
) -> (
    Entity<Project>,
    Entity<Project>,
    WindowHandle<MultiWorkspace>,
    Entity<Workspace>,
) {
    let app_state = cx.update(|cx| AppState::test(cx));
    let fs = FakeFs::new(cx.executor());
    let displayed_project = Project::test(fs.clone(), [], cx).await;
    let held_project = Project::test(fs, [], cx).await;
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
            |multi_workspace, window, cx| {
                let workspace =
                    cx.new(|cx| Workspace::new(Default::default(), project, app_state, window, cx));
                multi_workspace.add(workspace.clone(), window, cx);
                workspace
            }
        })
        .unwrap();
    cx.run_until_parked();
    (displayed_project, held_project, window, held_workspace)
}

#[gpui::test]
async fn test_long_activation_burst_activates_only_the_final_key_window(cx: &mut TestAppContext) {
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
    activate(&window_a, cx);
    settle(cx);
    assert!(is_active(&project_a, cx));
    assert!(!is_active(&project_b, cx));
    assert!(!is_active(&project_c, cx));
    cx.simulate_window_active_status_change(window_a.into(), false);
    cx.run_until_parked();
    assert!(!is_active(&project_a, cx));
    assert_eq!(cx.update(|cx| cx.active_window()), Some(window_a.into()));

    // When the application regains focus, AppKit makes A, then B, then C key. Each key
    // change renders a frame synchronously, so on a stalled main thread the gaps between
    // the callbacks are longer than any per-window settle delay.
    cx.simulate_window_active_status_change(window_a.into(), true);
    cx.run_until_parked();
    half_settle(cx);
    assert!(
        !is_active(&project_a, cx),
        "a window re-keyed early in a long burst must not activate its project"
    );
    assert!(!is_active(&project_b, cx));

    cx.simulate_window_active_status_change(window_b.into(), true);
    cx.run_until_parked();
    half_settle(cx);
    assert!(!is_active(&project_a, cx));
    assert!(
        !is_active(&project_b, cx),
        "a window re-keyed in the middle of a long burst must not activate its project"
    );

    activate(&window_c, cx);
    assert!(!is_active(&project_a, cx));
    assert!(!is_active(&project_b, cx));
    settle(cx);
    assert!(!is_active(&project_a, cx));
    assert!(!is_active(&project_b, cx));
    assert!(
        is_active(&project_c, cx),
        "only the window that is active once the burst has ended may activate its project"
    );
}

#[gpui::test]
async fn test_spurious_activation_of_background_window_does_not_activate_its_project(
    cx: &mut TestAppContext,
) {
    init_test(cx);

    let (_app_state, (project_a, window_a), (project_b, window_b)) =
        two_workspace_windows(cx).await;
    activate(&window_b, cx);
    settle(cx);
    assert_eq!(cx.update(|cx| cx.active_window()), Some(window_b.into()));
    assert!(!is_active(&project_a, cx));
    assert!(is_active(&project_b, cx));

    // macOS reports every window as active when the application regains focus, so
    // window A hears that it is active while window B remains the key window.
    cx.simulate_window_active_status_change(window_a.into(), true);
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| window_a.is_active(cx)), Some(true));
    settle(cx);
    assert!(
        !is_active(&project_a, cx),
        "a window that is not the platform's active window must not activate its project"
    );
    assert!(
        is_active(&project_b, cx),
        "the platform's active window must keep its project active"
    );
}

#[gpui::test]
async fn test_window_deactivation_reaches_project_immediately(cx: &mut TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    let project = Project::test(fs, [], cx).await;
    let (_workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
    settle(cx);
    assert!(is_active(&project, cx));

    cx.deactivate_window();
    assert!(
        !is_active(&project, cx),
        "deactivation must reach the project without waiting for the settle delay"
    );

    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert!(!is_active(&project, cx));
    cx.deactivate_window();
    assert!(!is_active(&project, cx));
    settle(cx);
    assert!(
        !is_active(&project, cx),
        "a deactivation during the settle delay must leave the project inactive"
    );
}

#[gpui::test]
async fn test_rapid_window_activations_coalesce(cx: &mut TestAppContext) {
    init_test(cx);

    let fs = FakeFs::new(cx.executor());
    let project = Project::test(fs, [], cx).await;
    let (_workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
    cx.deactivate_window();
    assert!(!is_active(&project, cx));

    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    half_settle(cx);
    assert!(!is_active(&project, cx));

    // A second activation of the same window arrives while the first is still pending.
    cx.simulate_active_status_change(true);
    cx.run_until_parked();
    half_settle(cx);
    assert!(
        !is_active(&project, cx),
        "a repeated activation must restart the settle delay rather than let the first fire"
    );

    half_settle(cx);
    assert!(
        is_active(&project, cx),
        "the coalesced activation must reach the project once the delay has elapsed"
    );
}

#[gpui::test]
async fn test_window_closed_during_activation_burst_keeps_decision_for_the_others(
    cx: &mut TestAppContext,
) {
    init_test(cx);

    let (app_state, (project_a, window_a), (project_b, window_b)) = two_workspace_windows(cx).await;
    activate(&window_b, cx);
    settle(cx);
    assert!(!is_active(&project_a, cx));
    assert!(is_active(&project_b, cx));

    activate(&window_a, cx);
    assert!(!is_active(&project_a, cx));
    assert!(!is_active(&project_b, cx));

    // Window B closes while the decision for the burst is still pending.
    window_b
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| window_b.is_active(cx)), None);

    settle(cx);
    assert!(
        is_active(&project_a, cx),
        "the remaining window must still get its decision"
    );
    assert!(!is_active(&project_b, cx));
    assert_eq!(
        app_state
            .workspace_store
            .read_with(cx, |store, _| store.workspaces().count()),
        1,
        "the closed workspace must be dropped from the store"
    );
}

#[gpui::test]
async fn test_window_activation_activates_only_the_workspace_the_window_displays(
    cx: &mut TestAppContext,
) {
    init_test(cx);

    let (project_a, project_b, window, _workspace_b) = multi_workspace_window(cx).await;
    window
        .read_with(cx, |multi_workspace, _| {
            assert_eq!(multi_workspace.workspaces().count(), 2);
        })
        .unwrap();

    activate(&window, cx);
    settle(cx);
    assert!(
        is_active(&project_a, cx),
        "the project of the workspace the window displays must activate"
    );
    assert!(
        !is_active(&project_b, cx),
        "a workspace the window holds in the background shares the window handle but must not activate its project"
    );
}

#[gpui::test]
async fn test_switching_the_displayed_workspace_moves_project_activation(cx: &mut TestAppContext) {
    init_test(cx);

    let (project_a, project_b, window, workspace_b) = multi_workspace_window(cx).await;
    activate(&window, cx);
    settle(cx);
    assert!(is_active(&project_a, cx));
    assert!(!is_active(&project_b, cx));

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
        !is_active(&project_b, cx),
        "a workspace switch must wait for the settle delay like a window activation"
    );

    settle(cx);
    assert!(
        !is_active(&project_a, cx),
        "the workspace that left the front of the window must deactivate its project"
    );
    assert!(
        is_active(&project_b, cx),
        "the workspace brought to the front of the window must activate its project"
    );
}

#[gpui::test]
async fn test_window_deactivation_reaches_the_projects_of_held_workspaces(cx: &mut TestAppContext) {
    init_test(cx);

    let (project_a, project_b, window, workspace_b) = multi_workspace_window(cx).await;
    activate(&window, cx);
    settle(cx);
    assert!(is_active(&project_a, cx));
    assert!(!is_active(&project_b, cx));

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
    assert!(is_active(&project_a, cx));

    cx.simulate_window_active_status_change(window.into(), false);
    cx.run_until_parked();
    assert!(
        !is_active(&project_a, cx),
        "deactivation must reach a workspace the window holds in the background without waiting for the settle delay"
    );
    assert!(
        !is_active(&project_b, cx),
        "deactivation must reach the workspace the window displays without waiting for the settle delay"
    );
}
