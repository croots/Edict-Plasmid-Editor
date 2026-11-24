//! The keyboard and mouse shortcuts listed on the Settings → Hotkeys page.
//!
//! This is a reference table, not the bindings themselves: the workspace actions are
//! bound in `workspace::bind_keys`, the sequence editor and terminal read keys in their
//! own `on_key` handlers. Keep this list in step with those when adding a shortcut.

/// A shortcut as shown on macOS and on other platforms.
pub struct Hotkey {
    pub mac: &'static str,
    pub other: &'static str,
    pub description: &'static str,
}

impl Hotkey {
    pub fn keys(&self) -> &'static str {
        if cfg!(target_os = "macos") { self.mac } else { self.other }
    }
}

pub struct Group {
    pub title: &'static str,
    pub hotkeys: &'static [Hotkey],
}

const fn hk(mac: &'static str, other: &'static str, description: &'static str) -> Hotkey {
    Hotkey { mac, other, description }
}

pub const GROUPS: &[Group] = &[
    Group {
        title: "GENERAL",
        hotkeys: &[
            hk("⌘N", "Ctrl+N", "New file"),
            hk("⌘O", "Ctrl+O", "Open file"),
            hk("⇧⌘O", "Ctrl+Shift+O", "Open folder"),
            hk("⌘S", "Ctrl+S", "Save"),
            hk("⇧⌘S", "Ctrl+Shift+S", "Save as"),
            hk("⌘E", "Ctrl+E", "Export as"),
            hk("⌘W", "Ctrl+W", "Close tab"),
            hk("⌘,", "Ctrl+,", "Settings"),
            hk("F11 / ⌃⌘F", "F11", "Toggle full screen"),
            hk("Esc", "Esc", "Close menu, dialog or notice"),
            hk("⌘Q", "Ctrl+Q", "Quit"),
        ],
    },
    Group {
        title: "PANES",
        hotkeys: &[
            hk("⌘\\", "Ctrl+\\", "Split right with a map"),
            hk("⇧⌘\\", "Ctrl+Shift+\\", "Split down with a map"),
            hk("⇧⌘W", "Ctrl+Shift+W", "Close pane"),
        ],
    },
    Group {
        title: "SEQUENCE EDITOR",
        hotkeys: &[
            hk("A C G T N …", "A C G T N …", "Type bases (replaces the selection)"),
            hk("← → ↑ ↓", "← → ↑ ↓", "Move the cursor (Shift extends the selection)"),
            hk("Home / End", "Home / End", "Start / end of row"),
            hk("⌘Home / ⌘End", "Ctrl+Home / Ctrl+End", "Start / end of sequence"),
            hk("PgUp / PgDn", "PgUp / PgDn", "Move ten rows"),
            hk("⌫ / Del", "Backspace / Del", "Delete backward / forward"),
            hk("⌘A", "Ctrl+A", "Select all"),
            hk("⌘C", "Ctrl+C", "Copy"),
            hk("⇧⌘C", "Ctrl+Shift+C", "Copy reverse complement"),
            hk("⌘X", "Ctrl+X", "Cut"),
            hk("⌘V", "Ctrl+V", "Paste"),
            hk("⌘Z", "Ctrl+Z", "Undo"),
            hk("⇧⌘Z / ⌘Y", "Ctrl+Shift+Z / Ctrl+Y", "Redo"),
            hk("⌘= / ⌘-", "Ctrl+= / Ctrl+-", "Larger / smaller text"),
            hk("Shift+click", "Shift+click", "Extend the selection"),
        ],
    },
    Group {
        title: "TERMINAL",
        hotkeys: &[
            hk("⌃⇧V", "Ctrl+Shift+V", "Paste"),
            hk("⌃= / ⌃-", "Ctrl+= / Ctrl+-", "Larger / smaller text"),
            hk("⌃0", "Ctrl+0", "Reset text size"),
            hk("⇧PgUp / ⇧PgDn", "Shift+PgUp / Shift+PgDn", "Scroll back / forward a page"),
            hk("Return", "Enter", "Restart the shell after it exits"),
        ],
    },
    Group {
        title: "3D STRUCTURE",
        hotkeys: &[
            hk("Drag", "Drag", "Rotate"),
            hk("Right-drag / ⇧Drag", "Right-drag / Shift+drag", "Pan"),
            hk("Scroll", "Scroll", "Zoom"),
            hk("Double-click", "Double-click", "Reset the view"),
            hk("Click a residue", "Click a residue", "Select its codons in the sequence"),
        ],
    },
];
