//! The VOPR's command line. A failure prints the seed that reproduces it.
#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use hypertiger_vopr::world::{Coverage, Durations, Outcome, run_seed};

const USAGE: &str = "usage: hypertiger-vopr [--seeds N] [--from SEED] [--faults-seconds S] \
[--quiet-seconds S] [--jobs N] [--determinism] [--no-coverage-gate] [--trace SEED]";

struct Options {
    seeds: u64,
    from: u64,
    faults_seconds: u64,
    quiet_seconds: u64,
    jobs: u64,
    determinism: bool,
    coverage_gate: bool,
    trace: Option<u64>,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        seeds: 100,
        from: 0,
        faults_seconds: 90,
        quiet_seconds: 30,
        jobs: 4,
        determinism: false,
        coverage_gate: true,
        trace: None,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--determinism" => options.determinism = true,
            "--no-coverage-gate" => options.coverage_gate = false,
            "--seeds" | "--from" | "--faults-seconds" | "--quiet-seconds" | "--jobs"
            | "--trace" => {
                let text = arguments.next().ok_or_else(|| format!("{flag} needs a number"))?;
                let value = text.parse::<u64>().map_err(|_| format!("{flag}: {text}"))?;
                match flag.as_str() {
                    "--seeds" => options.seeds = value,
                    "--from" => options.from = value,
                    "--faults-seconds" => options.faults_seconds = value,
                    "--quiet-seconds" => options.quiet_seconds = value,
                    "--trace" => options.trace = Some(value),
                    _ => options.jobs = value.max(1),
                }
            }
            other => return Err(format!("unknown flag {other}\n{USAGE}")),
        }
    }
    Ok(options)
}

fn run_campaign(options: &Options, durations: Durations) -> Vec<Outcome> {
    let next = AtomicU64::new(options.from);
    let end = options.from + options.seeds;
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..options.jobs {
            let sender = sender.clone();
            let next = &next;
            scope.spawn(move || {
                loop {
                    let seed = next.fetch_add(1, Ordering::Relaxed);
                    if seed >= end {
                        break;
                    }
                    if sender.send(run_seed(seed, durations)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let mut outcomes: Vec<Outcome> = receiver.iter().collect();
        outcomes.sort_by_key(|outcome| outcome.seed);
        outcomes
    })
}

fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let durations = Durations {
        faults_ms: options.faults_seconds * 1000,
        quiet_ms: options.quiet_seconds * 1000,
    };
    if let Some(seed) = options.trace {
        let outcome = hypertiger_vopr::world::World::new(seed).traced().run(durations);
        println!("{:?}\n{:#?}", outcome.violation, outcome.coverage);
        return ExitCode::SUCCESS;
    }
    // Engine assertions become reported failures, and the seed is what matters in them.
    std::panic::set_hook(Box::new(|_| {}));

    let outcomes = run_campaign(&options, durations);
    let mut coverage = Coverage::default();
    let mut failures = 0u64;
    for outcome in &outcomes {
        coverage.add(&outcome.coverage);
        if let Some(violation) = &outcome.violation {
            failures += 1;
            if failures <= 10 {
                println!("seed {} FAILED: {violation}", outcome.seed);
            }
        }
    }
    println!("{} seeds, {failures} failed", outcomes.len());
    println!("{coverage:#?}");

    if options.determinism {
        for outcome in &outcomes {
            let again = run_seed(outcome.seed, durations);
            if again.trace_digest != outcome.trace_digest {
                println!("seed {} is not deterministic", outcome.seed);
                return ExitCode::FAILURE;
            }
        }
        println!("determinism: every seed replayed to the same trace");
    }
    if failures > 0 {
        return ExitCode::FAILURE;
    }
    if options.coverage_gate {
        let missing = coverage.unexercised();
        if !missing.is_empty() {
            println!("coverage gate: this campaign never exercised {missing:?}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
