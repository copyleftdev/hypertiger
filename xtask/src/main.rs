//! The gate, written in the project's own language: TigerStyle's rule for tools.
//!
//!     cargo run -p xtask -- gate [--seeds N] [--no-canaries] [--mutants]
//!     cargo run -p xtask -- style
//!     cargo run -p xtask -- canaries [--only NAME]
//!
//! An exit status alone proves little, so each step states what it checked, and the campaign checks
//! its own coverage: a run that never entered a branch is not evidence about that branch.
#![forbid(unsafe_code)]
#![allow(clippy::exit)]

mod canaries;
mod style;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The cores a step may use: the standing limit for this machine, whatever a tool's default says. It
/// has reset under heavier work, including a sweep of this crate at 8 cores on 2026-11-05.
pub const JOBS: &str = "4";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let root = workspace_root();
    let result = match arguments.first().map(String::as_str) {
        Some("gate") => gate(&root, &arguments[1..]),
        Some("style") => style::check(&root),
        Some("canaries") => canaries::run(&root, &arguments[1..]),
        _ => {
            Err("usage: xtask gate [--seeds N] [--no-canaries] [--mutants] | style | canaries [--only NAME]".into())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("\nFAILED: {message}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().map_or_else(|| manifest.to_path_buf(), Path::to_path_buf)
}

pub fn step(name: &str) {
    println!("\n== {name}");
}

pub fn run(root: &Path, program: &str, arguments: &[&str]) -> Result<(), String> {
    let status = Command::new("nice")
        .arg("-n")
        .arg("19")
        .arg(program)
        .args(arguments)
        .current_dir(root)
        .status()
        .map_err(|error| format!("{program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} exited with {status}", arguments.join(" ")))
    }
}

fn flag_value(arguments: &[String], name: &str, default: u64) -> Result<u64, String> {
    let Some(position) = arguments.iter().position(|argument| argument == name) else {
        return Ok(default);
    };
    let text = arguments.get(position + 1).ok_or_else(|| format!("{name} needs a number"))?;
    text.parse().map_err(|_| format!("{name}: {text}"))
}

fn gate(root: &Path, arguments: &[String]) -> Result<(), String> {
    let seeds = flag_value(arguments, "--seeds", 2000)?;
    step("format");
    run(root, "cargo", &["fmt", "--all", "--check"])?;

    step("clippy: pedantic, warnings are errors");
    run(
        root,
        "cargo",
        &["clippy", "--workspace", "--all-targets", "-j", JOBS, "--", "-D", "warnings"],
    )?;

    style::check(root)?;

    step("the kernel builds with no standard library and no allocator");
    run(
        root,
        "cargo",
        &["build", "-p", "hypertiger", "--target", "x86_64-unknown-none", "-j", JOBS],
    )?;

    step("tests");
    run(root, "cargo", &["test", "--workspace", "-j", JOBS, "-q"])?;

    step("dependency policy");
    check_dependencies(root)?;

    step(&format!("VOPR: {seeds} seeds, every branch exercised"));
    run(root, "cargo", &["build", "--release", "-p", "hypertiger-vopr", "-j", JOBS])?;
    let vopr = target_binary(root, "hypertiger-vopr")?;
    run(root, &vopr, &["--seeds", &seeds.to_string(), "--jobs", JOBS])?;

    step("VOPR: determinism");
    run(root, &vopr, &["--from", "70000", "--seeds", "40", "--jobs", JOBS, "--determinism"])?;

    if arguments.iter().any(|argument| argument == "--no-canaries") {
        // Say so loudly: a run without canaries has not shown that the detectors above can detect.
        step("canaries: SKIPPED, so this run does not show the detectors can catch anything");
    } else {
        step("canaries: every planted bug must be caught");
        canaries::run(root, &[])?;
    }

    if arguments.iter().any(|argument| argument == "--mutants") {
        step("mutation testing, bounded");
        run_mutants(root)?;
    }
    println!("\ngate passed");
    Ok(())
}

fn run_mutants(root: &Path) -> Result<(), String> {
    run(root, "bounded-mutants", &["--package", "hypertiger", "--timeout", "300"])?;
    let missed = std::fs::read_to_string(root.join("mutants.out/missed.txt")).unwrap_or_default();
    let caught = std::fs::read_to_string(root.join("mutants.out/caught.txt")).unwrap_or_default();
    if caught.is_empty() {
        return Err("no mutants were caught, so the sweep did not really run".to_string());
    }
    if !missed.is_empty() {
        return Err(format!("mutants survived:\n{missed}"));
    }
    Ok(())
}

fn target_binary(root: &Path, name: &str) -> Result<String, String> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps", "--offline"])
        .current_dir(root)
        .output()
        .map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let key = "\"target_directory\":\"";
    let start = text.find(key).ok_or("no target directory in metadata")? + key.len();
    let end = text[start..].find('"').ok_or("unterminated target directory")? + start;
    Ok(format!("{}/release/{name}", &text[start..end]))
}

/// TigerBeetle allows no dependencies. This kernel allows cryptography, never hand-rolled, and the
/// edge allows TLS: see docs/STYLE.md. A new one has to be argued for here, in this list.
fn check_dependencies(root: &Path) -> Result<(), String> {
    for (crate_name, allowed) in [
        ("hypertiger", &["k256", "sha3"][..]),
        ("hypertiger-vopr", &["hypertiger"][..]),
        ("hypertiger-net", &["hypertiger", "rustls", "webpki-roots"][..]),
    ] {
        let manifest = root.join("crates").join(crate_name).join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest).map_err(|error| error.to_string())?;
        let mut in_dependencies = false;
        for line in text.lines() {
            if line.starts_with('[') {
                in_dependencies = line == "[dependencies]";
            } else if in_dependencies && !line.trim().is_empty() {
                let name = line.split(['.', '=', ' ']).next().unwrap_or_default();
                if !allowed.contains(&name) {
                    return Err(format!("{crate_name} depends on {name}, which is not allowed"));
                }
            }
        }
        println!("  {crate_name}: {}", allowed.join(", "));
    }
    Ok(())
}
