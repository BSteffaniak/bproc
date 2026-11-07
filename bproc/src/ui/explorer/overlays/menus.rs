//! Concrete menus (process context, sort, quick filters, export) and the
//! executor for their commands.
use super::super::actions::Action;
use super::super::hits::Hit;
use super::super::{ExplorerState, UiRequest};
use super::menu::{MenuCommand, MenuItem, MenuOverlay, column_label};
use super::signal::{is_system_root, without_self};
use super::{AffinityDialog, Overlay, ReniceDialog, SignalDialog};
use crate::procs::ProcessKey;
use crate::procs::actions::affinity_supported;
use crate::procs::actions::{ActionRequest, Signal};
use crate::procs::columns::ALL_COLUMNS;
use crate::procs::export::ExportFormat;
use crate::procs::model::ProcState;
use bmux_tui::geometry::Point;
use std::collections::BTreeSet;

pub fn process_menu(state: &ExplorerState, key: ProcessKey, at: Point) -> MenuOverlay {
    let row = state.store.get(&key);
    let marked = state.table.marked();
    let multi = marked.len() > 1 && marked.contains(&key);
    let targets = if multi { marked } else { vec![key] };
    let name = state.name_of(&key);
    let live = row.is_some_and(|r| !r.is_ghost());
    let any_live = targets
        .iter()
        .any(|k| state.store.get(k).is_some_and(|r| !r.is_ghost()));
    let header = if multi {
        format!("{} marked processes", targets.len())
    } else if live || row.is_none() {
        format!("{name} · PID {}", key.pid)
    } else {
        format!("{name} · PID {} (exited)", key.pid)
    };
    let stopped = row.is_some_and(|r| r.entry.sample.state == ProcState::Stopped);
    let throttled = row
        .and_then(|r| r.net.as_ref())
        .is_some_and(|n| n.throttle_limit.is_some());
    let has_tree = targets.iter().any(|k| !state.store.children(k).is_empty());
    // Killing launchd/init's tree would signal nearly every process.
    let system_root = targets.iter().any(|k| is_system_root(k.pid));
    let has_parent = !state.store.ancestors(&key).is_empty();

    let signals: Vec<MenuItem> = Signal::ALL
        .iter()
        .map(|s| {
            MenuItem::command(s.verb(), MenuCommand::Signal(*s))
                .hint(s.name())
                .enabled(any_live)
        })
        .collect();
    let (pause_label, pause_signal) = if stopped {
        ("Resume", Signal::Cont)
    } else {
        ("Suspend", Signal::Stop)
    };
    let pin_label = if multi {
        if targets.iter().all(|k| state.table.pinned().contains(k)) {
            "Unpin marked"
        } else {
            "Pin marked"
        }
    } else if state.table.pinned().contains(&key) {
        "Unpin"
    } else {
        "Pin to top"
    };
    let mark_label = if multi {
        "Unmark all"
    } else if state.table.marked().contains(&key) {
        "Unmark"
    } else {
        "Mark"
    };
    let user = row.and_then(|r| r.entry.record.user.clone());
    let command = row.map(|r| r.entry.record.command());
    let exe = row.and_then(|r| r.entry.record.exe.clone());
    let copy = |label: &str, text: Option<String>, what: &str| {
        let enabled = text.as_deref().is_some_and(|t| !t.is_empty());
        MenuItem::command(
            label,
            MenuCommand::Copy {
                text: text.unwrap_or_default(),
                what: what.to_string(),
            },
        )
        .enabled(enabled)
    };

    let mut items = vec![
        MenuItem::header(header),
        MenuItem::command("Details", MenuCommand::Run(Action::Details))
            .hint("Enter")
            .enabled(row.is_some()),
        MenuItem::command("Full screen details", MenuCommand::Run(Action::FullDetails))
            .hint("Shift+Enter")
            .enabled(row.is_some()),
        MenuItem::separator(),
        MenuItem::submenu("Signal", signals).hint("x"),
        MenuItem::command("Terminate", MenuCommand::Signal(Signal::Term))
            .hint("SIGTERM")
            .enabled(any_live),
        MenuItem::command("Force kill", MenuCommand::Signal(Signal::Kill))
            .hint("SIGKILL")
            .enabled(any_live),
        MenuItem::command(pause_label, MenuCommand::Signal(pause_signal))
            .hint(pause_signal.name())
            .enabled(any_live),
        MenuItem::command("Renice…", MenuCommand::Renice)
            .hint("F7 F8")
            .enabled(any_live),
        MenuItem::command("Kill process tree…", MenuCommand::SignalTree)
            .enabled(any_live && has_tree && !system_root),
    ];
    if affinity_supported() {
        items.push(MenuItem::command("Set CPU affinity…", MenuCommand::Affinity).enabled(any_live));
    }
    items.extend([
        MenuItem::separator(),
        MenuItem::command("Throttle bandwidth…", MenuCommand::Run(Action::Throttle))
            .hint("t")
            .enabled(live),
    ]);
    if throttled {
        items.push(
            MenuItem::command("Remove throttle", MenuCommand::Run(Action::RemoveThrottle))
                .hint("r"),
        );
    }
    items.extend([
        MenuItem::separator(),
        MenuItem::command(pin_label, MenuCommand::TogglePin).hint("P"),
        MenuItem::command(mark_label, MenuCommand::ToggleMark).hint("Space"),
        MenuItem::command(
            "Watch this process…",
            MenuCommand::Run(Action::WatchProcess),
        )
        .hint("W")
        .enabled(live),
        MenuItem::separator(),
        MenuItem::command(
            "Filter: same name",
            MenuCommand::Search(term("name", &name)),
        )
        .enabled(row.is_some()),
        MenuItem::command(
            "Filter: same user",
            MenuCommand::Search(term("user", user.as_deref().unwrap_or_default())),
        )
        .enabled(user.is_some()),
        MenuItem::command("Show in tree", MenuCommand::ShowInTree).enabled(row.is_some()),
        MenuItem::command("Go to parent", MenuCommand::Run(Action::GoParent))
            .hint("Backspace")
            .enabled(has_parent),
        MenuItem::separator(),
        MenuItem::submenu(
            "Copy",
            vec![
                copy("PID", Some(key.pid.to_string()), "PID"),
                copy("Name", row.map(|_| name.clone()), "name"),
                copy("Command line", command, "command line"),
                copy("Executable path", exe, "executable path"),
            ],
        ),
    ]);
    let mut menu = MenuOverlay::new(at, items);
    menu.subject = Some(key);
    menu.targets = targets;
    menu
}

pub fn sort_menu(state: &ExplorerState) -> MenuOverlay {
    let sort = state.table.sort();
    let visible = state.table.columns();
    let entry =
        |c| MenuItem::command(column_label(c), MenuCommand::Sort(c)).check(c == sort.column);
    let mut items = vec![MenuItem::header("Sort by")];
    items.extend(visible.iter().copied().map(entry));
    let more: Vec<MenuItem> = ALL_COLUMNS
        .iter()
        .copied()
        .filter(|c| !visible.contains(c))
        .map(entry)
        .collect();
    if !more.is_empty() {
        items.push(MenuItem::submenu("More", more));
    }
    items.push(MenuItem::separator());
    items.push(
        MenuItem::command("Descending", MenuCommand::ToggleDescending).toggle(sort.descending),
    );
    let mut menu = MenuOverlay::new(anchor(state, Some(Action::SortMenu)), items);
    // Start on the active column so Enter keeps it and arrows browse nearby.
    if let Some(i) = visible.iter().position(|c| *c == sort.column) {
        menu.cursor[0] = Some(i + 1);
    }
    menu
}

pub fn filter_menu(state: &ExplorerState) -> MenuOverlay {
    let s = &state.settings;
    let search = state.search.text.trim();
    let users: BTreeSet<String> = state
        .store
        .rows()
        .iter()
        .filter_map(|r| r.entry.record.user.clone())
        .collect();
    let user_items: Vec<MenuItem> = users
        .into_iter()
        .map(|u| {
            let t = term("user", &u);
            let on = search == t;
            MenuItem::command(u, MenuCommand::Search(t)).check(on)
        })
        .collect();
    let state_items: Vec<MenuItem> = [
        ProcState::Running,
        ProcState::Sleeping,
        ProcState::Stopped,
        ProcState::Zombie,
    ]
    .iter()
    .map(|p| {
        let t = format!("state:{}", p.code());
        let on = search == t;
        MenuItem::command(p.label(), MenuCommand::Search(t))
            .hint(p.code().to_string())
            .check(on)
    })
    .collect();
    let items = vec![
        MenuItem::header("Quick filters"),
        MenuItem::command("Only my processes", MenuCommand::Run(Action::ToggleUsers))
            .hint("u")
            .toggle(s.only_mine)
            .enabled(state.uid.is_some()),
        MenuItem::command(
            "Show kernel threads",
            MenuCommand::Run(Action::ToggleKernelThreads),
        )
        .hint("K")
        .toggle(!s.hide_kernel_threads),
        MenuItem::command(
            "Network-active only",
            MenuCommand::Run(Action::ToggleNetActive),
        )
        .hint("N")
        .toggle(s.net_active_only),
        MenuItem::command("Tree view", MenuCommand::Run(Action::Tree))
            .hint("F5")
            .toggle(s.tree),
        MenuItem::separator(),
        MenuItem::submenu("Filter by user", user_items),
        MenuItem::submenu("Filter by state", state_items),
        MenuItem::separator(),
        MenuItem::command("Clear search", MenuCommand::ClearSearch)
            .hint("Esc")
            .enabled(!state.search.text.is_empty()),
    ];
    MenuOverlay::new(anchor(state, Some(Action::FilterMenu)), items)
}

pub fn export_menu(state: &ExplorerState) -> MenuOverlay {
    let pids: Vec<String> = state
        .table
        .rows()
        .iter()
        .map(|r| r.key.pid.to_string())
        .collect();
    let items = vec![
        MenuItem::header("Export view"),
        MenuItem::command(
            "Export CSV",
            MenuCommand::Request(UiRequest::Export(ExportFormat::Csv)),
        ),
        MenuItem::command(
            "Export JSON",
            MenuCommand::Request(UiRequest::Export(ExportFormat::Json)),
        ),
        MenuItem::separator(),
        MenuItem::command(
            "Copy visible PIDs",
            MenuCommand::Copy {
                what: format!(
                    "{} PID{}",
                    pids.len(),
                    if pids.len() == 1 { "" } else { "s" }
                ),
                text: pids.join(" "),
            },
        )
        .enabled(!pids.is_empty()),
    ];
    MenuOverlay::new(anchor(state, None), items)
}

/// Where a keyboard-opened menu appears: above its F-key when that button is
/// on screen, otherwise at the top-left of the main area.
fn anchor(state: &ExplorerState, action: Option<Action>) -> Point {
    if let Some(action) = action
        && let Some(r) = state.hits.rect_of(|h| *h == Hit::FKey(action))
    {
        return Point::new(r.x, r.y);
    }
    let main = state.layout(state.last_area).main;
    Point::new(main.x.saturating_add(2), main.y.saturating_add(1))
}

/// `key:value`, quoted when the value contains spaces.
fn term(key: &str, value: &str) -> String {
    if value.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("{key}:\"{}\"", value.replace('"', ""))
    } else {
        format!("{key}:{value}")
    }
}

/// Execute `command` for a menu opened on `subject` acting on `targets`.
pub fn execute(
    command: &MenuCommand,
    subject: Option<ProcessKey>,
    targets: &[ProcessKey],
    state: &mut ExplorerState,
) {
    let targets: Vec<ProcessKey> = if targets.is_empty() {
        subject.into_iter().collect()
    } else {
        targets.to_vec()
    };
    match command {
        MenuCommand::Run(action) => {
            if let Some(key) = subject {
                state.table.select(key);
            }
            state.run(*action);
        }
        MenuCommand::Request(request) => state.request(request.clone()),
        MenuCommand::Copy { text, what } => {
            state.request(UiRequest::Copy(text.clone()));
            state.flash(format!("Copied {what}"));
        }
        MenuCommand::Search(text) => {
            state.search.set_text(text);
            state.screen = super::super::hits::Screen::Processes;
            state.table.mark_dirty();
        }
        MenuCommand::ClearSearch => {
            state.search.clear();
            state.table.mark_dirty();
        }
        MenuCommand::Signal(signal) => {
            let (targets, skipped_self) = without_self(targets);
            if skipped_self {
                warn_self(state);
            }
            if targets.is_empty() {
                return;
            }
            // Signals that end the process confirm first (SIGHUP usually reloads).
            let confirm =
                state.settings.confirm_signals && signal.is_terminating() && *signal != Signal::Hup;
            if confirm {
                let dialog = SignalDialog::new(state, targets, *signal);
                state.overlays.push(Overlay::Signal(dialog));
            } else {
                state.request(UiRequest::Action(ActionRequest::Signal {
                    targets,
                    signal: *signal,
                }));
            }
        }
        MenuCommand::SignalTree => {
            if targets.is_empty() || targets.iter().any(|k| is_system_root(k.pid)) {
                return;
            }
            let mut dialog = SignalDialog::new(state, targets, Signal::Term);
            dialog.include_children = !dialog.children.is_empty();
            state.overlays.push(Overlay::Signal(dialog));
        }
        MenuCommand::Renice => {
            if !targets.is_empty() {
                let dialog = ReniceDialog::new(state, targets, None);
                state.overlays.push(Overlay::Renice(dialog));
            }
        }
        MenuCommand::Affinity => {
            if !targets.is_empty() && affinity_supported() {
                let dialog = AffinityDialog::new(state, targets);
                state.overlays.push(Overlay::Affinity(dialog));
            }
        }
        MenuCommand::TogglePin => {
            if targets.len() > 1 {
                let all = targets.iter().all(|k| state.table.pinned().contains(k));
                for key in &targets {
                    if state.table.pinned().contains(key) == all {
                        state.table.toggle_pin(*key);
                    }
                }
                state.flash(format!(
                    "{} {} processes",
                    if all { "Unpinned" } else { "Pinned" },
                    targets.len()
                ));
            } else if let Some(key) = subject {
                let pinned = state.table.toggle_pin(key);
                state.flash(format!(
                    "{} {}",
                    if pinned { "Pinned" } else { "Unpinned" },
                    state.name_of(&key)
                ));
            }
        }
        MenuCommand::ToggleMark => {
            if targets.len() > 1 {
                state.table.clear_marks();
            } else if let Some(key) = subject {
                state.table.toggle_mark(key);
            }
        }
        MenuCommand::ShowInTree => {
            let Some(key) = subject else { return };
            if !state.settings.tree {
                state.settings.tree = true;
                state.request(UiRequest::SaveSettings);
            }
            for ancestor in state.store.ancestors(&key) {
                state.table.set_collapsed(ancestor, false);
            }
            state.screen = super::super::hits::Screen::Processes;
            state.table.select(key);
            state.table.mark_dirty();
            state.flash("Tree view");
        }
        MenuCommand::Sort(column) => {
            let current = state.table.sort();
            let descending = if current.column == *column {
                current.descending
            } else {
                column.default_descending()
            };
            set_sort(state, *column, descending);
        }
        MenuCommand::ToggleDescending => {
            let current = state.table.sort();
            set_sort(state, current.column, !current.descending);
        }
    }
}

/// Tell the user bproc left itself out of a signal.
fn warn_self(state: &mut ExplorerState) {
    state.toasts.push(
        super::super::toasts::Severity::Warning,
        format!(
            "bproc does not signal itself (PID {}). Press q to quit.",
            std::process::id()
        ),
    );
}

fn set_sort(state: &mut ExplorerState, column: crate::procs::columns::ColumnId, descending: bool) {
    state
        .table
        .set_sort(crate::procs::store::SortSpec { column, descending });
    state.settings.sort_column = Some(column);
    state.settings.sort_descending = descending;
    state.request(UiRequest::SaveSettings);
}
