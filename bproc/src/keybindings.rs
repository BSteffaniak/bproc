/// Centralized keybinding definitions for bproc
/// This ensures the help menu, status bar, and actual key handlers stay in sync

#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub key: &'static str,
    pub description: &'static str,
}

/// Get all keybindings
pub fn get_all_keybindings() -> Vec<KeyBinding> {
    vec![
        // Navigation
        KeyBinding {
            key: "↑/k",
            description: "Move selection up",
        },
        KeyBinding {
            key: "↓/j",
            description: "Move selection down",
        },
        KeyBinding {
            key: "i",
            description: "Toggle interface view",
        },
        KeyBinding {
            key: "l",
            description: "Cycle traffic view (All/Internet/Local)",
        },
        KeyBinding {
            key: "Enter",
            description: "View details (process or interface)",
        },
        KeyBinding {
            key: "Tab",
            description: "Switch tabs (in detail view)",
        },
        KeyBinding {
            key: "Space",
            description: "Toggle interface filter (in interface list)",
        },
        KeyBinding {
            key: "A",
            description: "Toggle All/None interfaces (in interface list)",
        },
        // Actions
        KeyBinding {
            key: "t",
            description: "Throttle selected process",
        },
        KeyBinding {
            key: "r",
            description: "Remove throttle",
        },
        KeyBinding {
            key: "g",
            description: "Toggle bandwidth graph",
        },
        KeyBinding {
            key: "f",
            description: "Freeze/unfreeze sort order",
        },
        // System
        KeyBinding {
            key: "b",
            description: "View/switch backends",
        },
        KeyBinding {
            key: "h/?",
            description: "Toggle this help",
        },
        KeyBinding {
            key: "q/Esc",
            description: "Quit (or close modal if open)",
        },
        KeyBinding {
            key: "Ctrl+C",
            description: "Force quit (always exits)",
        },
    ]
}

/// Get keybindings for the status bar (most common ones)
pub fn get_status_bar_keybindings() -> Vec<(&'static str, &'static str)> {
    vec![
        ("↑↓", "Navigate"),
        ("i", "Interfaces"),
        ("l", "Traffic"),
        ("t", "Throttle"),
        ("f", "Freeze"),
        ("b", "Backends"),
        ("h", "Help"),
        ("q/Ctrl+C", "Quit"),
    ]
}
