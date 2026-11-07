# bproc

**A mouse-first terminal process explorer with per-process network monitoring and experimental bandwidth control, in Rust.**

bproc is an htop-class process monitor and manager. Every process is listed with CPU, memory, disk and network columns. You can sort, filter, mark, signal, renice, inspect and watch processes from the keyboard or entirely with the mouse. Captured traffic is attributed to processes, so the same table shows per-process bandwidth. On Linux, optional backends enforce upload and download limits through cgroups, eBPF and traffic-control tools. Scriptable `ps`, `tree`, `signal` and `watch` subcommands share the same columns, filter language and watch rules as the TUI.

> **Experimental software.** Monitoring attribution and rate enforcement depend on the OS, kernel, privileges, network topology and selected backend. This is not a security boundary and does not guarantee exact accounting. Test policies in a disposable Linux environment before applying them to important workloads.

## Capabilities

**Process explorer (the main screen)**

- A process table with column presets (Overview, CPU, Memory, Disk, Network, Custom), resizable and reorderable columns, list or tree view, collapsible subtrees, pinned rows, and a "freeze sort order" mode.
- Meters header: CPU (total and per core), memory, swap, load, tasks, network, disk and uptime. Clicking a meter switches to the matching preset.
- Search and filter box with a small query language, e.g. `node cpu>5 -user:root`, plus quick filters: only my processes, hide kernel threads, network-active only.
- Signals (TERM, KILL, HUP, INT, QUIT, STOP, CONT, USR1, USR2) and renice. Before signalling, the process identity (PID plus start time) is checked, so a reused PID is never hit. Processes that ignore SIGTERM are offered a one-click force kill. SIGHUP is treated as a reload and never triggers that offer.
- A detail inspector, docked at the bottom or right or shown full screen, with tabs for Overview (sparklines), Threads, Files, Network, Environment, Memory, Tree, Limits and Events. Its breadcrumbs and links are clickable.
- An Events timeline (process starts, exits, alerts and actions) and a Watches screen of alert rules. A rule watches a metric such as CPU, memory, RSS, disk, network, threads or fds for a target process, name, command or user. When the condition holds for long enough, the rule logs, notifies or sends a signal.
- Export of the current view to CSV or JSON.

**Network**

- Per-process download and upload rates and totals, interface views and filters, and bandwidth history graphs.
- Local/internet traffic classification. Here, **local** means private, loopback, link-local, unspecified or IPv4 limited-broadcast address space. It does not mean physical proximity or a routing-table lookup.
- Per-process throttling (Linux): eBPF ingress/egress token buckets with cgroup-scoped configuration, and alternative HTB, IFB, police and nftables backends behind Cargo features.

The default build enables monitoring only. macOS has a monitoring backend but no bandwidth-control backend. On Windows, per-process traffic is polled without packet capture, and the Linux enforcement guidance below does not apply.

## Build

Install Git and stable Rust. Packet capture requires OS-specific privileges. Linux throttling also needs writable cgroup facilities and the tools or modules for the chosen backend (`tc` from iproute2, nftables, or IFB as applicable).

```sh
git clone https://github.com/BSteffaniak/bproc.git
cd bproc

# Monitoring only
cargo build --locked --release -p bproc
./target/release/bproc --help
```

### Linux eBPF build

The cgroup-SKB backend checks for cgroup v2 and Linux 4.10 or later. That check is only preliminary: kernel configuration, attach permissions and verifier acceptance still matter.

```sh
rustup toolchain install nightly --component rust-src
cargo install bpf-linker --locked

# Compile optimized BPF objects, then the release application
cargo xtask build-release --features linux-full
```

`cargo xtask build-ebpf` builds only the optimized BPF objects. Both debug and release application builds take those objects from `target/bpfel-unknown-none/release`. If you run plain `cargo build --features linux-full` before building them, the eBPF backend is unavailable and the build emits a warning.

For a build without eBPF:

```sh
cargo build --locked --release -p bproc \
  --features throttle-tc-htb,throttle-ifb-tc,cgroup-v1,cgroup-v2-nftables
```

IFB is required only for the IFB download backend, not for every download implementation. Fallback backends may not have the same semantics as the requested one, so check the reported capabilities and active policies.

## Usage

### Interactive explorer

```sh
./target/release/bproc            # process explorer; network columns need capture privileges
sudo ./target/release/bproc       # adds per-process bandwidth (and throttling on Linux)
```

**Mouse.** Everything is clickable:

- **Rows.** Click to select, double-click for details, right-click for a context menu of every action. Ctrl/Alt-click marks a row, Shift-click marks a range, middle-click toggles a mark.
- **Headers.** Click to sort, drag a header edge to resize, double-click an edge to reset its width, right-click to choose columns.
- **Meters, presets and filter chips.** Click to switch.
- **F-key bar.** Click any key.
- **Detail panel.** Drag the splitter to resize it. Click tabs, buttons and process links.
- **Wheel and scrollbar.** Scroll anywhere. Drag the scrollbar.

Hovering over the table briefly holds the row order so rows do not move under the pointer (Settings → Hover freeze).

**Keyboard.** The keys follow htop where they overlap:

| Key | Action |
| --- | --- |
| F1 / ? | Help |
| F2 / S | Settings |
| F3 / / | Search |
| F4 | Quick filters |
| F5 / T | Tree view |
| F6 / > | Sort menu |
| F7 / F8 | Nice − / + |
| F9 / x | Send signal |
| F10 / q | Quit |
| Enter / Shift+Enter | Open details / full-screen details |
| . | Actions menu |
| Space / c / U | Mark / mark with children / clear marks |
| Backspace | Go to the parent process |
| ] [ | Next / previous preset |
| 1 2 3 | Processes / Events / Watches screens |
| W | Watch this process |
| t / r / g | Throttle / remove throttle / bandwidth graph |
| i / l / b | Interfaces / traffic mode / throttle backends |

**Targets.** Actions apply to the highlighted process. Marks apply only when the highlighted row is itself marked, and this rule is the same for keys, F-keys and menus. When an action would reach marked rows that the current filter hides, bproc always asks for confirmation. On the Events and Watches screens, process keys act on the selected event's or alert's process, never on hidden table marks. In the full-screen inspector they act on the inspected process.

**Watch rules.** A rule can send a signal, so guardrails apply. bproc never signals itself, its ancestors (your shell and terminal) or PID 0/1. Those firings, and any beyond a per-rule limit for one sample, are downgraded to notifications.

### Unprivileged vs sudo

Without root, the explorer shows every process you can see. Details that other users' processes keep private are marked as limited. Per-process bandwidth needs packet-capture privileges. bproc shows a note when capture seems unavailable and removes it once traffic is attributed, for example on macOS with ChmodBPF access. Throttling needs root on Linux.

Under `sudo`, bproc reads and writes the invoking user's config, not root's, and hands created files back to that user. Exports work the same way. So an elevated session shares your rules, columns and saved throttles, and leaves no root-owned files behind.

### Command-line subcommands

```sh
bproc ps --sort mem -n 20                      # aligned table; --json / --csv for scripts
bproc ps --filter 'node cpu>5 -user:root' -c pid,user,cpu,mem,command
bproc tree                                     # process hierarchy (same options as ps)
bproc signal 4242 --signal term --tree         # verified identity; --tree includes descendants
bproc watch --name 'node*' --metric cpu --above 80 --for 30s --action notify
```

Plain-text output replaces control characters and bidi overrides in process names and command lines with `?`, so another user's argv cannot inject terminal escape sequences. `bproc signal --tree` fails rather than signal only the root when it cannot read the process table. Legacy flags remain for headless throttling (`--pid` with `--download-limit` and/or `--upload-limit`) and for `--list-backends`.

### Throttling

Use the throttle dialog (`t`, or Throttle in a row's menu) to set limits for a process. Applying a policy can move processes into cgroups and change kernel networking state, so test on a disposable workload first and remove the policy when you are done. bproc checks the process's identity when the dialog opens and again right before applying, and refuses exited or reused PIDs.

```sh
sudo ./target/release/bproc --no-restore --no-save   # evaluation: don't restore or save throttles
```

By default, throttles active at exit are saved and restored on the next start. A saved throttle is re-applied to the same process (same PID and start time). Otherwise it goes to exactly one process with the same executable and command line. An entry that matches several processes, or has no executable path, is skipped with a warning and never applied to every process with the same name. `--no-save` affects throttles only. Explorer settings and watch rules are still saved.

### Configuration

Settings are stored in `throttles.json` under the platform config directory: `~/.config/bproc` on Linux, `~/Library/Application Support/bproc` on macOS, and `%APPDATA%\bproc` on Windows. Besides saved throttles and backend preferences, the file holds:

- `explorer`: preset, per-preset columns, column widths, sort, tree view, refresh interval, quick filters, dock position and size, meters, heat colors, hover freeze, signal confirmation and the force-kill delay.
- `watch_rules`: alert rules.
- `filtered_interfaces` and `traffic_view_mode`: the network views.

Saves are atomic: the file is written to a temporary file and then renamed. Values that this build cannot read are ignored individually, for example a column or rule written by a newer version. If the whole file cannot be parsed, it is renamed to `throttles.json.bak-<time>` and you are warned, rather than having it silently overwritten.

## Architecture and limitations

- [`bproc`](bproc) owns monitoring, process/socket attribution, the UI, the CLI and backend selection:
  - `src/procs/`: the process domain (model, platform samplers, collector, store, columns, filter language, tree, history, actions, deep detail, watch rules, export). It does no terminal I/O.
  - `src/ui/explorer/`: the explorer UI. Painters are pure; side effects are queued as requests that the runtime executes off the UI thread.
  - `src/tui_runtime.rs`: event routing, effect execution and the legacy network dialogs. Kernel and config changes are serialized through one service thread.
- [`bproc-common`](bproc-common) defines the userspace/BPF map types and the tested packet-address classification.
- [`bproc-ebpf`](bproc-ebpf) contains the separately compiled ingress, egress and TC programs.
- [`xtask`](xtask) builds the BPF objects before the application embeds them.

Packet capture and socket enumeration are separate operations. Short-lived sockets, shared sockets, capture loss and unmapped traffic can prevent exact per-process attribution. The monitor tracks unmatched packets, but it does not count every byte with guaranteed accuracy.

The eBPF backend drops packets when a token bucket is exhausted: it polices traffic rather than queueing packets. Download policing happens after traffic reaches the machine, so it cannot guarantee less upstream link utilization. Cgroup inheritance and existing sockets also need workload-specific testing.

## Development and validation

```sh
cargo fmt --all
cargo test --locked --workspace
cargo clippy --workspace --all-targets
cargo build --locked -p bproc --no-default-features   # monitoring-free build must stay green
```

Host unit tests do not validate Linux kernel enforcement. See [Linux validation](docs/linux-validation.md) for the required runtime matrix. To report a bug, open a [GitHub Issue](https://github.com/BSteffaniak/bproc/issues) with the OS/kernel, build features, selected backend and a non-sensitive reproduction.

### Terminal UI tests

The interactive frontend uses BMUX components and its scheduling/presentation runtime. BMUX dependencies track GitHub `master`, and the workspace `Cargo.lock` records the exact resolved revision. Refresh that resolution deliberately and validate it as a unit.

Most UI behaviour is covered by headless tests that paint the explorer into an in-memory buffer and drive it with synthetic key and mouse events through the same `handle_event` entry point as the terminal. They check:

- **Routing:** clicks, drags, wheel and double-clicks, and which process every action targets on each screen.
- **Layout:** tiny and huge terminals, and that every visible control is hit-testable.
- **Overlays and dialogs:** menus, the signal and renice dialogs, the watch editor and settings.
- **Runtime:** effect admission and ordering, throttle-identity checks and config persistence.

The unprivileged PTY smoke test drives a real terminal with an isolated, monitoring-free build:

```sh
cargo build --locked -p bproc --no-default-features
python3 scripts/smoke-tui.py "$PWD/target/debug/bproc"
```

The harness isolates configuration and disables policy restoration and persistence. It checks normal and Ctrl+C exit, resizing, tiny terminals and terminal-mode restoration. It does not exercise packet capture or real throttling. Rebuild with default features before evaluating monitoring.

## License

[MIT](LICENSE). BMUX dependencies are MPL-2.0 licensed; retain their license and source notices when distributing the application.
