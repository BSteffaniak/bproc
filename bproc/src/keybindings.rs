//! Keys of the legacy interface views (interface filter modal and interface
//! detail). The process explorer has its own registry in
//! `ui::explorer::actions`; these rows only describe what `tui_runtime::input`
//! handles while an interface view owns input.

#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub key: &'static str,
    pub description: &'static str,
}

const fn bind(key: &'static str, description: &'static str) -> KeyBinding {
    KeyBinding { key, description }
}

/// Every binding available in the interface views, for the help overlay.
pub fn get_all_keybindings() -> Vec<KeyBinding> {
    vec![
        bind("↑/k ↓/j", "Move the interface cursor / scroll processes"),
        bind("PgUp PgDn", "Scroll by a page"),
        bind("Space", "Toggle the interface filter (or click its box)"),
        bind("A", "Toggle all / no interfaces"),
        bind("Enter", "Show processes on the interface"),
        bind("Esc q", "Back (detail → list → process explorer)"),
        bind("i", "Close the interface views"),
        bind("l", "Cycle traffic view (All/Internet/Local)"),
        bind("b", "Throttle backends"),
        bind("h ?", "Toggle this help"),
        bind("Click", "Select; click outside a panel to close it"),
        bind("Ctrl+C", "Quit bproc"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_are_unique_and_described() {
        let all = get_all_keybindings();
        let mut keys = std::collections::HashSet::new();
        for binding in &all {
            assert!(!binding.description.is_empty());
            assert!(keys.insert(binding.key), "duplicate {}", binding.key);
        }
    }
}
