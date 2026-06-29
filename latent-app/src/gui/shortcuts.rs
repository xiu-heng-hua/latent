//! The single source of truth for keyboard shortcuts.
//!
//! One table ([`SHORTCUTS`]) lists every binding as an `{ action, keys,
//! description }` row. The input dispatcher in [`crate::gui::app`] reads it to map
//! a key press to an [`Action`], and the cheat-sheet renders its rows — so the
//! help is generated from the same list the handler dispatches from, never a
//! separately-maintained copy.
//!
//! **Focus gating is the load-bearing correctness rule.** A bare-letter or
//! bracket binding (`c`, `b`, `[`, `]`, `0`, `1`, `` ` ``, `Tab`, `?`) must not
//! fire while a text field, numeric entry, or name field holds keyboard focus —
//! otherwise typing a name would switch tools. Each row records whether it
//! requires the command modifier; a bare (non-command) binding is suppressed when
//! a widget is focused, while command-modified bindings stay live.

use eframe::egui::{self, Key, Modifiers};

/// An action a shortcut can invoke. Each is also reachable from a button or menu;
/// the shortcut and the widget are two front-ends to the same `App` method, never
/// divergent logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Open,
    Export,
    Undo,
    Redo,
    Copy,
    Paste,
    ResetAll,
    ZoomFit,
    ZoomActual,
    ZoomIn,
    ZoomOut,
    BeforeAfter,
    TogglePanel,
    ToggleHelp,
    BrushSmaller,
    BrushLarger,
    NextVariant,
    PrevVariant,
    ToolCrop,
    ToolBrush,
    ApplyTool,
    CancelTool,
}

/// Whether a binding needs a key modifier, so a bare-letter binding can be
/// suppressed while a text field is focused while a `Cmd/Ctrl` one stays live.
/// The `Shift`-for-feather refinement on the brush keys is read live at apply
/// time, not modeled as a separate binding here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mods {
    /// No modifier — a bare key. Suppressed while a widget holds keyboard focus.
    Bare,
    /// The command modifier (`Cmd` on macOS, `Ctrl` elsewhere). Stays live while
    /// typing.
    Command,
    /// Command plus Shift.
    CommandShift,
}

impl Mods {
    /// Whether this binding is safe while a text widget is focused.
    fn safe_while_typing(self) -> bool {
        matches!(self, Mods::Command | Mods::CommandShift)
    }

    fn to_egui(self) -> Modifiers {
        match self {
            Mods::Bare => Modifiers::NONE,
            Mods::Command => Modifiers::COMMAND,
            Mods::CommandShift => Modifiers::COMMAND | Modifiers::SHIFT,
        }
    }

    /// Whether the pressed `modifiers` satisfy this binding. Command bindings match
    /// **exactly** on shift so `Cmd+Z` (undo) and `Cmd+Shift+Z` (redo) stay
    /// distinct; bare bindings match **logically** so a key that needs shift to be
    /// produced (`+`) still fires.
    fn matches(self, modifiers: Modifiers) -> bool {
        let pattern = self.to_egui();
        match self {
            Mods::Bare => modifiers.matches_logically(pattern),
            Mods::Command | Mods::CommandShift => modifiers.matches_exact(pattern),
        }
    }
}

/// One shortcut row: the action it triggers, the key + modifier it binds, the
/// printable key label for the cheat-sheet, and a description.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shortcut {
    pub(crate) action: Action,
    key: Key,
    mods: Mods,
    pub(crate) keys: &'static str,
    pub(crate) description: &'static str,
}

/// The complete shortcut set — the one definition both the dispatcher and the
/// cheat-sheet consume. A binding that is not in this table does not exist.
pub(crate) const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        action: Action::Open,
        key: Key::O,
        mods: Mods::Command,
        keys: "Cmd/Ctrl + O",
        description: "Open a RAW file",
    },
    Shortcut {
        action: Action::Export,
        key: Key::E,
        mods: Mods::Command,
        keys: "Cmd/Ctrl + E",
        description: "Export the current image",
    },
    Shortcut {
        action: Action::Undo,
        key: Key::Z,
        mods: Mods::Command,
        keys: "Cmd/Ctrl + Z",
        description: "Undo",
    },
    Shortcut {
        action: Action::Redo,
        key: Key::Z,
        mods: Mods::CommandShift,
        keys: "Cmd/Ctrl + Shift + Z  /  Cmd/Ctrl + Y",
        description: "Redo",
    },
    // The second redo binding (Cmd/Ctrl + Y); shares the description with the row
    // above, so the cheat-sheet shows it once.
    Shortcut {
        action: Action::Redo,
        key: Key::Y,
        mods: Mods::Command,
        keys: "",
        description: "",
    },
    Shortcut {
        action: Action::Copy,
        key: Key::C,
        mods: Mods::Command,
        keys: "Cmd/Ctrl + C",
        description: "Copy develop settings",
    },
    Shortcut {
        action: Action::Paste,
        key: Key::V,
        mods: Mods::Command,
        keys: "Cmd/Ctrl + V",
        description: "Paste develop settings",
    },
    Shortcut {
        action: Action::ResetAll,
        key: Key::Backspace,
        mods: Mods::Command,
        keys: "Cmd/Ctrl + Backspace",
        description: "Reset all develop adjustments",
    },
    Shortcut {
        action: Action::ZoomFit,
        key: Key::Num0,
        mods: Mods::Bare,
        keys: "0",
        description: "Zoom to fit",
    },
    Shortcut {
        action: Action::ZoomActual,
        key: Key::Num1,
        mods: Mods::Bare,
        keys: "1",
        description: "Zoom to 100%",
    },
    Shortcut {
        action: Action::ZoomIn,
        key: Key::Plus,
        mods: Mods::Bare,
        keys: "+  /  −",
        description: "Zoom in / out",
    },
    // Second zoom-in binding (the `=` key, so Shift is not needed for `+`); shares
    // the row above.
    Shortcut {
        action: Action::ZoomIn,
        key: Key::Equals,
        mods: Mods::Bare,
        keys: "",
        description: "",
    },
    Shortcut {
        action: Action::ZoomOut,
        key: Key::Minus,
        mods: Mods::Bare,
        keys: "",
        description: "",
    },
    Shortcut {
        action: Action::BeforeAfter,
        key: Key::Backtick,
        mods: Mods::Bare,
        keys: "`",
        description: "Cycle before / after view",
    },
    Shortcut {
        action: Action::BrushSmaller,
        key: Key::OpenBracket,
        mods: Mods::Bare,
        keys: "[  /  ]",
        description: "Brush smaller / larger (Shift for feather)",
    },
    Shortcut {
        action: Action::BrushLarger,
        key: Key::CloseBracket,
        mods: Mods::Bare,
        keys: "",
        description: "",
    },
    Shortcut {
        action: Action::ToolCrop,
        key: Key::C,
        mods: Mods::Bare,
        keys: "C",
        description: "Crop tool",
    },
    Shortcut {
        action: Action::ToolBrush,
        key: Key::B,
        mods: Mods::Bare,
        keys: "B",
        description: "Brush tool",
    },
    Shortcut {
        action: Action::PrevVariant,
        key: Key::Comma,
        mods: Mods::Bare,
        keys: ",  /  .",
        description: "Previous / next variant",
    },
    Shortcut {
        action: Action::NextVariant,
        key: Key::Period,
        mods: Mods::Bare,
        keys: "",
        description: "",
    },
    Shortcut {
        action: Action::ApplyTool,
        key: Key::Enter,
        mods: Mods::Bare,
        keys: "Enter",
        description: "Apply the active tool",
    },
    Shortcut {
        action: Action::CancelTool,
        key: Key::Escape,
        mods: Mods::Bare,
        keys: "Esc",
        description: "Cancel the active tool",
    },
    Shortcut {
        action: Action::TogglePanel,
        key: Key::Tab,
        mods: Mods::Bare,
        keys: "Tab",
        description: "Hide / show the controls panel",
    },
    Shortcut {
        action: Action::ToggleHelp,
        key: Key::Questionmark,
        mods: Mods::Bare,
        keys: "?",
        description: "Show this shortcut list",
    },
];

/// The rows the cheat-sheet renders: each table entry that carries a non-empty key
/// label (the secondary bindings share their primary's row, so they are skipped),
/// as `(keys, description)` pairs. Pure over the table so the row set is testable
/// without a window.
pub(crate) fn cheat_sheet_rows() -> Vec<(&'static str, &'static str)> {
    SHORTCUTS
        .iter()
        .filter(|s| !s.keys.is_empty())
        .map(|s| (s.keys, s.description))
        .collect()
}

/// Collect the actions whose binding fired this frame, reading the table. A
/// `focused` widget suppresses every bare (non-command) binding so a single letter
/// never fires while typing into a text field, numeric entry, or name field;
/// command-modified bindings stay live. Pure over the egui input snapshot, so the
/// focus gate is testable via [`is_suppressed`].
pub(crate) fn fired_actions(i: &egui::InputState, focused: bool) -> Vec<Action> {
    SHORTCUTS
        .iter()
        .filter(|s| !is_suppressed(s.mods, focused))
        .filter(|s| i.key_pressed(s.key) && s.mods.matches(i.modifiers))
        .map(|s| s.action)
        .collect()
}

/// Whether a binding with the given modifiers is suppressed because a widget holds
/// keyboard focus. Bare and bare-shift bindings are suppressed while focused;
/// command-modified ones are not. Split out as a pure function so the load-bearing
/// focus gate is unit-tested directly.
pub(crate) fn is_suppressed(mods_safe_while_typing: impl SafeWhileTyping, focused: bool) -> bool {
    focused && !mods_safe_while_typing.safe_while_typing()
}

/// Trait so [`is_suppressed`] can take either a private [`Mods`] or, in tests, a
/// plain bool answer for "safe while typing".
pub(crate) trait SafeWhileTyping {
    fn safe_while_typing(&self) -> bool;
}

impl SafeWhileTyping for Mods {
    fn safe_while_typing(&self) -> bool {
        (*self).safe_while_typing()
    }
}

impl SafeWhileTyping for bool {
    fn safe_while_typing(&self) -> bool {
        *self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheat_sheet_renders_from_the_table() {
        // The cheat-sheet rows come straight from the table (secondary bindings,
        // with an empty key label, are folded into their primary's row).
        let rows = cheat_sheet_rows();
        let descs: Vec<&str> = rows.iter().map(|(_, d)| *d).collect();
        assert!(descs.contains(&"Undo"));
        assert!(descs.contains(&"Redo"));
        assert!(descs.contains(&"Open a RAW file"));
        assert!(descs.contains(&"Export the current image"));
        assert!(descs.contains(&"Hide / show the controls panel"));
        assert!(descs.contains(&"Crop tool"));
        assert!(descs.contains(&"Brush tool"));
        // No empty rows leak through.
        assert!(rows.iter().all(|(k, _)| !k.is_empty()));
        // Adding a row flows through automatically (the table is the only source).
        assert_eq!(
            rows.len(),
            SHORTCUTS.iter().filter(|s| !s.keys.is_empty()).count()
        );
    }

    #[test]
    fn letter_shortcuts_suppressed_when_text_focused() {
        // The load-bearing focus gate: a bare letter / bracket binding does not
        // fire while a text field holds focus, but a command-modified one does.
        // Unfocused: a bare binding is live.
        assert!(!is_suppressed(false, false), "bare key live when unfocused");
        // Focused: a bare binding is suppressed (so `C`/`B`/`[` never fire while
        // typing a variant name or a preset name or a numeric entry).
        assert!(is_suppressed(false, true), "bare key gated while typing");
        // A command-modified binding stays live even while focused.
        assert!(
            !is_suppressed(true, true),
            "command shortcut works while typing"
        );
        assert!(!is_suppressed(true, false));

        // And the table's bare tool/brush bindings really are gated.
        for s in SHORTCUTS {
            if matches!(
                s.action,
                Action::ToolCrop | Action::ToolBrush | Action::BrushSmaller | Action::BrushLarger
            ) {
                assert!(
                    is_suppressed(s.mods, true),
                    "{:?} must be gated while typing",
                    s.action
                );
            }
            if matches!(s.action, Action::Undo | Action::Redo | Action::Copy) {
                assert!(
                    !is_suppressed(s.mods, true),
                    "{:?} (command) must stay live while typing",
                    s.action
                );
            }
        }
    }

    #[test]
    fn every_action_has_a_binding() {
        // A quick consistency sweep: the redo and zoom rows carry their secondary
        // key bindings (Y, =, −, ]) with an empty cheat-sheet label, folded into
        // the primary row. So the table has more entries than cheat-sheet rows.
        assert!(SHORTCUTS.len() > cheat_sheet_rows().len());
    }
}
