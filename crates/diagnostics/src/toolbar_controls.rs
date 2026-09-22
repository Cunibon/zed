use crate::{
    BufferDiagnosticsEditor, ProjectDiagnosticsEditor, SetMaxSeverity, ToggleDiagnosticsRefresh,
    set_panel_max_severity,
};
use agent_settings::AgentSettings;
use gpui::{Action as _, Context, EventEmitter, ParentElement, Render, Window};
use language::DiagnosticEntry;
use project::project_settings::DiagnosticSeverity;
use settings::Settings;
use std::sync::Arc;
use text::{Anchor, BufferId};
use ui::{ContextMenu, PopoverMenu, Tooltip, prelude::*};
use workspace::{ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, item::ItemHandle};
use zed_actions::assistant::InlineAssist;
use zed_actions::buffer_search;

pub struct ToolbarControls {
    editor: Option<Arc<dyn DiagnosticsToolbarEditor>>,
}

pub(crate) trait DiagnosticsToolbarEditor: Send + Sync {
    /// The most lenient severity the diagnostics editor currently shows.
    fn max_severity(&self, cx: &App) -> DiagnosticSeverity;
    /// Indicates whether the diagnostics editor is currently updating the
    /// diagnostics.
    fn is_updating(&self, cx: &App) -> bool;
    /// Requests that the diagnostics editor stop updating the diagnostics.
    fn stop_updating(&self, cx: &mut App);
    /// Requests that the diagnostics editor updates the displayed diagnostics
    /// with the latest information.
    fn refresh_diagnostics(&self, window: &mut Window, cx: &mut App);
    /// Returns a list of diagnostics for the provided buffer id.
    fn get_diagnostics_for_buffer(
        &self,
        buffer_id: BufferId,
        cx: &App,
    ) -> Vec<DiagnosticEntry<Anchor>>;
}

impl Render for ToolbarControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut max_severity = DiagnosticSeverity::Warning;
        let mut is_updating = false;

        match &self.editor {
            Some(editor) => {
                max_severity = editor.max_severity(cx);
                is_updating = editor.is_updating(cx);
            }
            None => {}
        }

        let is_agent_enabled = AgentSettings::get_global(cx).enabled(cx);
        let editor = self.editor.clone();

        h_flex()
            .gap_1()
            .child({
                IconButton::new("toggle_search", IconName::MagnifyingGlass)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::for_action_title(
                        "Buffer Search",
                        &buffer_search::Deploy::find(),
                    ))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(buffer_search::Deploy::find()), cx);
                    })
            })
            .when(is_agent_enabled, |this| {
                this.child(
                    IconButton::new("inline_assist", IconName::ZedAssistant)
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::for_action_title(
                            "Inline Assist",
                            &InlineAssist::default(),
                        ))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(InlineAssist::default()), cx);
                        }),
                )
            })
            .map(|div| {
                if is_updating {
                    div.child(
                        IconButton::new("stop-updating", IconName::Stop)
                            .icon_color(Color::Error)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::for_action_title(
                                "Stop Diagnostics Update",
                                &ToggleDiagnosticsRefresh,
                            ))
                            .on_click(cx.listener(move |toolbar_controls, _, _, cx| {
                                if let Some(editor) = toolbar_controls.editor() {
                                    editor.stop_updating(cx);
                                    cx.notify();
                                }
                            })),
                    )
                } else {
                    div.child(
                        IconButton::new("refresh-diagnostics", IconName::ArrowCircle)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::for_action_title(
                                "Refresh Diagnostics",
                                &ToggleDiagnosticsRefresh,
                            ))
                            .on_click(cx.listener({
                                move |toolbar_controls, _, window, cx| {
                                    if let Some(editor) = toolbar_controls.editor() {
                                        editor.refresh_diagnostics(window, cx)
                                    }
                                }
                            })),
                    )
                }
            })
            .child(
                PopoverMenu::new("diagnostics-max-severity")
                    .trigger_with_tooltip(
                        IconButton::new("diagnostics-max-severity-trigger", IconName::Filter)
                            .icon_size(IconSize::Small),
                        Tooltip::text(format!(
                            "Showing {}",
                            max_severity_description(max_severity)
                        )),
                    )
                    .anchor(gpui::Anchor::TopRight)
                    .menu(move |window, cx| {
                        let editor = editor.clone()?;
                        let max_severity = editor.max_severity(cx);
                        Some(ContextMenu::build(window, cx, |mut menu, _, _| {
                            for (label, severity) in MAX_SEVERITY_ENTRIES {
                                menu = menu.toggleable_entry(
                                    label,
                                    max_severity >= severity,
                                    IconPosition::Start,
                                    Some(
                                        SetMaxSeverity {
                                            severity: severity.into(),
                                        }
                                        .boxed_clone(),
                                    ),
                                    move |_, cx| {
                                        set_panel_max_severity(
                                            clicked_max_severity(max_severity, severity),
                                            cx,
                                        )
                                    },
                                );
                            }
                            menu
                        }))
                    }),
            )
    }
}

const MAX_SEVERITY_ENTRIES: [(&str, DiagnosticSeverity); 4] = [
    ("Errors", DiagnosticSeverity::Error),
    ("Warnings", DiagnosticSeverity::Warning),
    ("Info", DiagnosticSeverity::Info),
    ("Hints", DiagnosticSeverity::Hint),
];

fn max_severity_description(max_severity: DiagnosticSeverity) -> &'static str {
    match max_severity {
        DiagnosticSeverity::Off | DiagnosticSeverity::Error => "Errors Only",
        DiagnosticSeverity::Warning => "Errors and Warnings",
        DiagnosticSeverity::Info => "Errors, Warnings, and Info",
        DiagnosticSeverity::Hint => "All Diagnostics",
    }
}

/// Every level at or above the threshold renders checked, so clicking the
/// threshold itself reads as unchecking it and moves the threshold one step
/// stricter. Errors can never be unchecked.
fn clicked_max_severity(
    current: DiagnosticSeverity,
    clicked: DiagnosticSeverity,
) -> DiagnosticSeverity {
    if clicked != current {
        return clicked;
    }
    match current {
        DiagnosticSeverity::Hint => DiagnosticSeverity::Info,
        DiagnosticSeverity::Info => DiagnosticSeverity::Warning,
        _ => DiagnosticSeverity::Error,
    }
}

impl EventEmitter<ToolbarItemEvent> for ToolbarControls {}

impl ToolbarItemView for ToolbarControls {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        if let Some(pane_item) = active_pane_item.as_ref() {
            if let Some(editor) = pane_item.downcast::<ProjectDiagnosticsEditor>() {
                self.editor = Some(Arc::new(editor.downgrade()));
                ToolbarItemLocation::PrimaryRight
            } else if let Some(editor) = pane_item.downcast::<BufferDiagnosticsEditor>() {
                self.editor = Some(Arc::new(editor.downgrade()));
                ToolbarItemLocation::PrimaryRight
            } else {
                ToolbarItemLocation::Hidden
            }
        } else {
            ToolbarItemLocation::Hidden
        }
    }
}

impl Default for ToolbarControls {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolbarControls {
    pub fn new() -> Self {
        ToolbarControls { editor: None }
    }

    fn editor(&self) -> Option<&dyn DiagnosticsToolbarEditor> {
        self.editor.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicking_the_current_threshold_makes_it_one_step_stricter() {
        assert_eq!(
            clicked_max_severity(DiagnosticSeverity::Hint, DiagnosticSeverity::Hint),
            DiagnosticSeverity::Info
        );
        assert_eq!(
            clicked_max_severity(DiagnosticSeverity::Info, DiagnosticSeverity::Info),
            DiagnosticSeverity::Warning
        );
        assert_eq!(
            clicked_max_severity(DiagnosticSeverity::Warning, DiagnosticSeverity::Warning),
            DiagnosticSeverity::Error
        );
        assert_eq!(
            clicked_max_severity(DiagnosticSeverity::Error, DiagnosticSeverity::Error),
            DiagnosticSeverity::Error
        );
    }

    #[test]
    fn clicking_another_level_selects_it() {
        assert_eq!(
            clicked_max_severity(DiagnosticSeverity::Error, DiagnosticSeverity::Hint),
            DiagnosticSeverity::Hint
        );
        assert_eq!(
            clicked_max_severity(DiagnosticSeverity::Hint, DiagnosticSeverity::Warning),
            DiagnosticSeverity::Warning
        );
    }
}
