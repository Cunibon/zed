---
title: Diagnostics - Errors and Warnings in Zed
description: View and navigate errors, warnings, and code diagnostics from language servers in Zed.
---

# Diagnostics

Zed gets its diagnostics from the language servers and supports both push and pull variants of the LSP which makes it compatible with all existing language servers.

# Regular diagnostics

By default, Zed displays all diagnostics as underlined text in the editor and the scrollbar.

Editor diagnostics could be filtered with the

```json [settings]
"diagnostics_max_severity": null
```

editor setting (possible values: `"off"`, `"error"`, `"warning"`, `"info"`, `"hint"`, `null` (default, all diagnostics)).

The scrollbar ones are configured with the

```json [settings]
"scrollbar": {
  "diagnostics": "all",
}
```

configuration (possible values: `"none"`, `"error"`, `"warning"`, `"information"`, `"all"` (default))

The diagnostics could be hovered to display a tooltip with a full, rendered diagnostic message.
Or, `editor::GoToDiagnostic` and `editor::GoToPreviousDiagnostic` could be used to navigate between diagnostics in the editor, showing a popover for the currently active diagnostic.

# Inline diagnostics (Error lens)

Zed supports showing diagnostics as a lens to the right of the code.
This is disabled by default, but can either be temporarily turned on (or off) using the editor menu, or permanently, using the

```json [settings]
"diagnostics": {
  "inline": {
    "enabled": true,
    "max_severity": null, // same values as the `diagnostics_max_severity` from the editor settings
  }
}
```

# Diagnostics panel

The project diagnostics panel (`diagnostics::Deploy`) and the buffer diagnostics view (`diagnostics::DeployCurrentFile`) list every diagnostic up to a severity threshold.
By default only errors and warnings are shown; lower the threshold to also see the info and hint diagnostics that language servers report:

```json [settings]
"diagnostics": {
  "max_severity": "warning" // "error", "warning", "info", "hint" or "all"
}
```

The threshold can also be changed for the current session, without editing settings, from the severity menu in the panel toolbar, with `diagnostics::ToggleWarnings`, or by binding `diagnostics::SetMaxSeverity` to a key:

```json [keymap]
{
  "context": "Diagnostics",
  "bindings": {
    "alt-i": ["diagnostics::SetMaxSeverity", { "severity": "info" }]
  }
}
```

The status bar only counts errors and warnings, so files that contain nothing but info or hint diagnostics do not add to its counters even though they appear in the panel.

# Other UI places

## Project Panel

Project panel can have its entries coloured based on the severity of the diagnostics in the file.

To configure, use

```json [settings]
"project_panel": {
  "show_diagnostics": "all",
}
```

configuration (possible values: `"off"`, `"errors"`, `"all"` (default))

## Editor tabs

Similar to the project panel, editor tabs can be colorized with the

```json [settings]
"tabs": {
  "show_diagnostics": "off",
}
```

configuration (possible values: `"off"` (default), `"errors"`, `"all"`)
