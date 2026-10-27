//! bproc Build Tasks
//!
//! This binary provides build automation for bproc, particularly for
//! building eBPF programs which require nightly Rust.
//!
//! Usage:
//!   cargo xtask build-ebpf    # Build eBPF programs only
//!   cargo xtask build         # Build everything (eBPF + main)
//!   cargo xtask build-release # Build release binaries
//!   cargo xtask clean         # Clean build artifacts

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build options parsed from command line
#[derive(Debug)]
struct BuildOptions {
    features: Option<String>,
    no_default_features: bool,
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();

    // Parse build options
    let build_opts = parse_build_args(&args);

    // BPF objects must be optimized and use the release path consumed by build.rs,
    // even when the userspace application is built in debug mode.
    match args.first().map(|s| s.as_str()) {
        Some("build-ebpf") => build_ebpf(true)?,
        Some("build") => {
            build_ebpf(true)?;
            build_main(false, &build_opts)?;
        }
        Some("build-release") => {
            build_ebpf(true)?;
            build_main(true, &build_opts)?;
        }
        Some("clean") => clean()?,
        Some("help") | Some("--help") | Some("-h") => print_help(),
        _ => {
            print_help();
            anyhow::bail!("No command specified");
        }
    }

    Ok(())
}

/// Parse build-related arguments from command line
fn parse_build_args(args: &[String]) -> BuildOptions {
    let mut features = None;
    let mut no_default_features = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--features" => {
                if let Some(feat) = args.get(i + 1) {
                    features = Some(feat.clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--no-default-features" => {
                no_default_features = true;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    BuildOptions {
        features,
        no_default_features,
    }
}

/// Build feature list ensuring throttle-ebpf is ALWAYS included
fn build_feature_list(opts: &BuildOptions) -> String {
    let mut features = vec!["throttle-ebpf".to_string()];

    if let Some(user_features) = &opts.features {
        // Parse user features (could be comma-separated)
        for feat in user_features.split(',') {
            let feat = feat.trim().to_string();
            // Don't duplicate throttle-ebpf if user specified it
            if feat != "throttle-ebpf" && !feat.is_empty() {
                features.push(feat);
            }
        }
    }

    features.join(",")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Strategy for invoking nightly Rust
#[derive(Debug, Clone)]
enum NightlyStrategy {
    /// Plain cargo is already using nightly (common in NixOS)
    PlainCargoIsNightly,
    /// Need to explicitly use nightly cargo and rustc
    UseNightlyToolchain {
        cargo_path: PathBuf,
        rustc_path: PathBuf,
    },
}

/// Detect how to invoke nightly Rust on this system
fn ensure_nightly_available() -> Result<NightlyStrategy> {
    // Strategy 1: Check if plain rustc is already nightly
    if let Ok(output) = Command::new("rustc").arg("--version").output()
        && output.status.success()
    {
        let version = String::from_utf8_lossy(&output.stdout);
        if version.contains("nightly") {
            println!("✓ Default rustc is nightly ({})", version.trim());
            return Ok(NightlyStrategy::PlainCargoIsNightly);
        }
    }

    // Strategy 2: Check if `rustc +nightly` works and get its path
    if let Ok(output) = Command::new("rustc")
        .args(["+nightly", "--version"])
        .output()
        && output.status.success()
    {
        let version = String::from_utf8_lossy(&output.stdout);
        if version.contains("nightly") {
            println!("✓ rustc +nightly is available ({})", version.trim());

            // Get the actual path to nightly toolchain
            // Use `rustc +nightly --print sysroot` to find it
            if let Ok(sysroot_output) = Command::new("rustc")
                .args(["+nightly", "--print", "sysroot"])
                .output()
            {
                let sysroot = String::from_utf8_lossy(&sysroot_output.stdout);
                let sysroot = sysroot.trim();
                let rustc_path = PathBuf::from(sysroot).join("bin/rustc");
                let cargo_path = PathBuf::from(sysroot).join("bin/cargo");

                if rustc_path.exists() && cargo_path.exists() {
                    println!("  Using nightly toolchain at: {}", sysroot);
                    return Ok(NightlyStrategy::UseNightlyToolchain {
                        cargo_path,
                        rustc_path,
                    });
                } else if rustc_path.exists() {
                    // Only rustc found, cargo might be a wrapper - use default cargo with RUSTC
                    println!("  Using rustc at: {}", rustc_path.display());
                    // Fall through to try finding cargo
                }
            }
        }
    }

    // Strategy 3: Try rustup if available
    if let Ok(output) = Command::new("rustup").arg("--version").output()
        && output.status.success()
    {
        println!("ℹ️  Checking rustup for nightly toolchain...");

        // Check if nightly is installed
        if let Ok(list_output) = Command::new("rustup").args(["toolchain", "list"]).output() {
            let toolchains = String::from_utf8_lossy(&list_output.stdout);

            if !toolchains.contains("nightly") {
                println!("⚠️  Nightly not found, installing via rustup...");

                let status = Command::new("rustup")
                    .args(["toolchain", "install", "nightly", "--component", "rust-src"])
                    .status()
                    .context("Failed to run rustup")?;

                if !status.success() {
                    anyhow::bail!("Failed to install nightly via rustup");
                }

                println!("✓ Nightly installed successfully");
            }

            // Get toolchain paths from rustup
            if let Ok(rustc_which) = Command::new("rustup")
                .args(["which", "--toolchain", "nightly", "rustc"])
                .output()
                && rustc_which.status.success()
            {
                let rustc_path = String::from_utf8_lossy(&rustc_which.stdout);
                let rustc_path = PathBuf::from(rustc_path.trim());

                if let Ok(cargo_which) = Command::new("rustup")
                    .args(["which", "--toolchain", "nightly", "cargo"])
                    .output()
                    && cargo_which.status.success()
                {
                    let cargo_path = String::from_utf8_lossy(&cargo_which.stdout);
                    let cargo_path = PathBuf::from(cargo_path.trim());
                    println!("  Using nightly toolchain from rustup");
                    return Ok(NightlyStrategy::UseNightlyToolchain {
                        cargo_path,
                        rustc_path,
                    });
                }
            }
        }
    }

    // Strategy 4: Nothing worked - show helpful error
    anyhow::bail!(
        "Nightly Rust not found!\n\
         \n\
         Please install nightly Rust using ONE of these methods:\n\
         \n\
         Method 1: rustup (most common)\n\
         \x20\x20rustup toolchain install nightly --component rust-src\n\
         \n\
         Method 2: NixOS configuration.nix\n\
         \x20\x20environment.systemPackages = with pkgs; [\n\
         \x20\x20\x20\x20(rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {{\n\
         \x20\x20\x20\x20\x20\x20extensions = [ \"rust-src\" ];\n\
         \x20\x20\x20\x20}}))\n\
         \x20\x20];\n\
         \n\
         Method 3: NixOS shell.nix (per-project)\n\
         \x20\x20{{ pkgs ? import <nixpkgs> {{}} }}:\n\
         \x20\x20pkgs.mkShell {{\n\
         \x20\x20\x20\x20buildInputs = with pkgs; [\n\
         \x20\x20\x20\x20\x20\x20rustc\n\
         \x20\x20\x20\x20\x20\x20cargo\n\
         \x20\x20\x20\x20\x20\x20bpf-linker\n\
         \x20\x20\x20\x20];\n\
         \x20\x20\x20\x20RUSTC_VERSION = pkgs.lib.readFile ./rust-toolchain.toml;\n\
         \x20\x20}}\n\
         \n\
         Method 4: Nix flakes with rust-overlay\n\
         \x20\x20See: https://github.com/oxalica/rust-overlay"
    );
}

fn build_ebpf(release: bool) -> Result<()> {
    let root = workspace_root();
    let ebpf_dir = root.join("bproc-ebpf");

    println!("🔨 Building eBPF programs...");

    // Check if bpf-linker is installed
    if Command::new("bpf-linker")
        .arg("--version")
        .output()
        .is_err()
    {
        anyhow::bail!(
            "bpf-linker not found!\n\
             \n\
             Install with:\n\
               cargo install bpf-linker\n\
             \n\
             Or on NixOS:\n\
               nix-env -iA nixpkgs.bpf-linker"
        );
    }

    // Determine nightly strategy
    let strategy = ensure_nightly_available()?;

    // Build all eBPF programs
    for (name, bin) in &[
        ("egress", "bproc-egress"),
        ("ingress", "bproc-ingress"),
        ("tc_classifier", "bproc-tc-classifier"),
    ] {
        println!("  → Building {}...", bin);

        let mut cmd = Command::new("cargo");
        cmd.current_dir(&ebpf_dir);

        // Configure cargo based on nightly strategy
        match &strategy {
            NightlyStrategy::PlainCargoIsNightly => {
                // Plain cargo is already nightly, just use build
                cmd.arg("build");
            }
            NightlyStrategy::UseNightlyToolchain {
                cargo_path,
                rustc_path,
            } => {
                // Use the nightly cargo directly instead of the wrapper
                // This ensures both cargo and rustc are from the nightly toolchain
                cmd = Command::new(cargo_path);
                cmd.current_dir(&ebpf_dir);
                cmd.env("RUSTC", rustc_path);
                cmd.arg("build");
            }
        }

        cmd.args(["--target=bpfel-unknown-none"])
            .args(["-Z", "build-std=core"])
            .args(["--bin", bin])
            .env("RUSTFLAGS", "-C link-arg=--disable-memory-builtins");

        if release {
            cmd.arg("--release");
        }

        let status = cmd
            .status()
            .with_context(|| format!("Failed to run cargo for {}", name))?;

        if !status.success() {
            anyhow::bail!("Failed to build {}", name);
        }
    }

    println!("✅ eBPF programs built successfully");
    Ok(())
}

fn build_main(release: bool, opts: &BuildOptions) -> Result<()> {
    let root = workspace_root();

    println!("🔨 Building main crate...");

    // Build feature list with throttle-ebpf ALWAYS included
    let features = build_feature_list(opts);

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&root).args(["build"]);

    // Apply no-default-features if requested
    if opts.no_default_features {
        cmd.arg("--no-default-features");
        println!("  → Disabling default features");
    }

    // Apply features (throttle-ebpf always included)
    if !features.is_empty() {
        cmd.args(["--features", &features]);
        println!("  → Using features: {}", features);
    }

    if release {
        cmd.arg("--release");
    }

    let status = cmd.status().context("Failed to run cargo for main crate")?;

    if !status.success() {
        anyhow::bail!("Main build failed");
    }

    println!("✅ Build complete");

    if release {
        let binary_path = root.join("target/release/bproc");
        println!("\n📦 Release binary at: {}", binary_path.display());
    }

    Ok(())
}

fn clean() -> Result<()> {
    let root = workspace_root();

    println!("🧹 Cleaning build artifacts...");

    let status = Command::new("cargo")
        .current_dir(&root)
        .args(["clean"])
        .status()
        .context("Failed to run cargo clean")?;

    if !status.success() {
        anyhow::bail!("Clean failed");
    }

    println!("✅ Clean complete");
    Ok(())
}

fn print_help() {
    println!("bproc Build Tasks\n");
    println!("USAGE:");
    println!("  cargo xtask <COMMAND> [OPTIONS]\n");
    println!("COMMANDS:");
    println!("  build-ebpf       Build eBPF programs only (requires nightly + bpf-linker)");
    println!("  build            Build everything (eBPF + main crate) in debug mode");
    println!("  build-release    Build everything in release mode");
    println!("  clean            Clean all build artifacts");
    println!("  help             Show this help message\n");
    println!("OPTIONS:");
    println!("  --features <FEATURES>      Enable specific features (comma-separated)");
    println!("  --no-default-features      Disable default features\n");
    println!("EXAMPLES:");
    println!("  cargo xtask build");
    println!("    → Default build with monitor-pnet + throttle-ebpf\n");
    println!("  cargo xtask build --features linux-full");
    println!("    → Build with all Linux backends enabled\n");
    println!("  cargo xtask build --features \"throttle-tc-htb,throttle-ifb-tc\"");
    println!("    → Build with specific throttle backends\n");
    println!("  cargo xtask build --no-default-features --features throttle-tc-htb");
    println!("    → Build without monitor-pnet, but with TC HTB backend\n");
    println!("  cargo xtask build-release --features linux-full");
    println!("    → Production build with all Linux features\n");
    println!("  cargo xtask build-ebpf");
    println!("    → Just rebuild eBPF programs\n");
    println!("NOTES:");
    println!("  - throttle-ebpf is ALWAYS included (required for eBPF programs)");
    println!("  - Default features: monitor-pnet (can disable with --no-default-features)");
    println!("  - Feature bundles: linux-full, macos-full");
    println!("  - See bproc/Cargo.toml for all available features\n");
    println!("REQUIREMENTS:");
    println!("  - Rust nightly (auto-detected or auto-installed)");
    println!("  - bpf-linker: cargo install bpf-linker");
    println!("  - Linux kernel 4.15+ with eBPF support\n");
    println!("ENVIRONMENT SUPPORT:");
    println!("  - Works with rustup (auto-installs nightly if needed)");
    println!("  - Works with NixOS (detects nightly automatically)");
    println!("  - Works with nix-shell and flakes");
}
