# bproc

**Per-process network monitoring and experimental bandwidth control in Rust.**

bproc associates captured traffic with processes and presents usage in a terminal UI or CLI. On Linux, optional backends enforce upload and download limits through cgroups, eBPF, and traffic-control tools.

> **Experimental software.** Monitoring attribution and rate enforcement depend on the OS, kernel, privileges, network topology, and selected backend. This is not a security boundary or a guarantee of exact accounting. Test policies on a disposable Linux environment before applying them to important workloads.

## Capabilities

- Process and interface bandwidth views, connection details, and usage history.
- Interactive and non-interactive control of per-process policies.
- Linux eBPF ingress/egress token buckets with cgroup-scoped configuration.
- Alternative HTB, IFB, police, and nftables backends behind Cargo features.
- Local/internet traffic classification. Here, **local** means private, loopback, link-local, unspecified, or IPv4 limited-broadcast address space—not physical proximity or a routing-table lookup.

The default build enables monitoring only. macOS has a monitoring backend but no implemented bandwidth-control backend. Windows-specific monitoring code exists; it is not covered by the Linux enforcement guidance below.

## Build

Install Git and stable Rust. Packet capture requires OS-specific privileges. Linux throttling additionally needs writable cgroup facilities and the tools/modules for the chosen backend (`tc` from iproute2, nftables, or IFB as applicable).

```sh
git clone https://github.com/BSteffaniak/bproc.git
cd bproc

# Monitoring only
cargo build --locked --release -p bproc
./target/release/bproc --help
```

### Linux eBPF build

The cgroup-SKB backend checks for cgroup v2 and Linux 4.10 or later. That check is only a preliminary capability check: kernel configuration, attach permissions, and verifier acceptance still matter.

```sh
rustup toolchain install nightly --component rust-src
cargo install bpf-linker --locked

# Compile optimized BPF objects, then the release application
cargo xtask build-release --features linux-full
```

`cargo xtask build-ebpf` builds the optimized BPF objects alone. Both debug and release application builds consume those objects from `target/bpfel-unknown-none/release`. Running plain `cargo build --features linux-full` before building them leaves the eBPF backend unavailable and emits a warning.

For a build without eBPF:

```sh
cargo build --locked --release -p bproc \
  --features throttle-tc-htb,throttle-ifb-tc,cgroup-v1,cgroup-v2-nftables
```

IFB is required only for the IFB download backend, not for every download implementation. Do not assume fallback provides the same semantics as the requested backend; inspect the reported capabilities and active policies.

## Usage

Start with `./target/release/bproc --help` for the current CLI. On Linux, run monitoring with the privileges needed for packet capture:

```sh
sudo ./target/release/bproc --no-restore --no-save
```

These flags avoid restoring saved policies or saving this evaluation session.
Without them, saved throttles are restored by default.

Use the TUI's throttle dialog to select a process and limits. Applying a policy can move processes into cgroups and modify kernel networking state. Use a test workload first and remove the policy when finished.

## Architecture and limitations

- [`bproc`](bproc) owns monitoring, process/socket attribution, UI, CLI, and backend selection.
- [`bproc-common`](bproc-common) defines the userspace/BPF map types and tested packet-address classification.
- [`bproc-ebpf`](bproc-ebpf) contains the separately compiled ingress, egress, and TC programs.
- [`xtask`](xtask) builds BPF objects before the application embeds them.

Packet capture and socket enumeration are separate operations. Short-lived sockets, shared sockets, capture loss, and unmapped traffic can prevent exact per-process attribution. The monitor tracks unmatched packets; it does not count every byte with guaranteed accuracy.

The eBPF backend drops packets when a token bucket is exhausted; it is policing, not a packet queue. Download policing happens after traffic reaches the machine and cannot guarantee a reduction in upstream link utilization. Cgroup inheritance and existing sockets also require workload-specific testing.

## Development and validation

```sh
cargo fmt --all
cargo test --locked --workspace
cargo clippy --workspace --all-targets
```

Host unit tests do not validate Linux kernel enforcement. See [Linux validation](docs/linux-validation.md) for the required runtime matrix. Report bugs with the OS/kernel, build features, selected backend, and a non-sensitive reproduction through [GitHub Issues](https://github.com/BSteffaniak/bproc/issues).

### Terminal UI checks

The interactive frontend uses BMUX components and its scheduling/presentation runtime.
BMUX dependencies track GitHub `master`; the workspace `Cargo.lock` records the exact
resolved revision. Refresh that resolution deliberately and validate it as a unit.
Application-specific presentation components live under `bproc/src/ui`.

Run the unprivileged PTY lifecycle smoke test with an isolated monitoring-free build:

```sh
cargo build --locked -p bproc --no-default-features
python3 scripts/smoke-tui.py "$PWD/target/debug/bproc"
```

The harness isolates configuration, disables policy restoration/persistence, checks
normal and Ctrl+C exit, resizing, tiny terminals, and terminal-mode restoration.
It does not exercise packet capture, real throttling, or prove visual parity.
Restore the default-feature build before evaluating monitoring.

The parity suite compares 33 non-graph screens with captured pre-migration
terminal-cell glyphs. Wide-cell continuation representation and the repaired
80-column header border are explicit exceptions. Graph tests cover plot geometry,
Braille output, and series colors; BMUX's rasterization is not bit-for-bit identical.

## License

[MIT](LICENSE). BMUX dependencies are MPL-2.0 licensed; retain their license and
source notices when distributing the application.
