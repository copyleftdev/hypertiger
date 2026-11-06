//! Bugs planted on purpose. A gate that cannot catch these is not evidence of anything.
//!
//! Each canary changes a few lines in a copy of the tree and must make the named detector fail.
//! Planting in a copy matters: on this machine a hard reset once left a planted bug in a working
//! tree. A canary whose text no longer matches the source fails the run too, so a refactor cannot
//! silently retire one.

use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy)]
enum Detector {
    /// The kernel's own test suite.
    Tests,
    /// The VOPR, within this many seeds.
    Vopr(u64),
}

struct Canary {
    name: &'static str,
    /// What was broken, in terms of what a user would lose.
    meaning: &'static str,
    file: &'static str,
    find: &'static str,
    replace: &'static str,
    detector: Detector,
}

const ENGINE: &str = "crates/hypertiger/src/engine.rs";
const WORLD: &str = "crates/hypertiger-vopr/src/world.rs";

const CANARIES: &[Canary] = &[
    Canary {
        name: "forget_on_timeout",
        meaning: "a timed-out order stops counting, though it may have landed",
        file: ENGINE,
        find: "            slot.state = State::Ambiguous;\n            slot.poll_ms = 0;\n",
        replace: "            slot.state = State::Ambiguous;\n            slot.poll_ms = 0;\n            slot.sz_filled = slot.sz;\n",
        detector: Detector::Vopr(400),
    },
    Canary {
        name: "retry_on_timeout",
        meaning: "a timed-out order is sent again: two orders where one was wanted",
        file: ENGINE,
        find: "            slot.state = State::Ambiguous;\n            slot.poll_ms = 0;\n",
        replace: "            slot.state = State::Queued;\n            slot.deadline_ms = self.now_ms;\n            slot.poll_ms = 0;\n",
        detector: Detector::Vopr(400),
    },
    Canary {
        name: "ask_status_early",
        meaning: "an ambiguous order is queried at once, and \"unknown\" concluded from it",
        file: ENGINE,
        find: "                self.now_ms >= slot.deadline_ms + u64::from(self.config.settle_margin_ms)\n",
        replace: "                true\n",
        detector: Detector::Tests,
    },
    Canary {
        name: "no_margin",
        meaning: "the deadline ignores clock skew and stale reads",
        file: WORLD,
        find: "settle_margin_ms: 1000,",
        replace: "settle_margin_ms: 1,",
        detector: Detector::Vopr(2000),
    },
    Canary {
        name: "no_expiry",
        meaning: "orders carry no expiry, so a late landing can follow a proof it never would",
        file: ENGINE,
        find: "let size = self.write_signed(&action, nonce, Some(expires_ms), signer, target);",
        replace: "let size = self.write_signed(&action, nonce, None, signer, target);",
        detector: Detector::Vopr(400),
    },
    Canary {
        name: "no_fill_dedupe",
        meaning: "a replayed snapshot counts fills twice",
        file: ENGINE,
        find: "        if self.fills_seen.contains(&key) {\n            return;\n        }\n",
        replace: "",
        detector: Detector::Tests,
    },
    Canary {
        name: "fills_out_of_order",
        meaning: "a newest-first list is applied in the order it arrived",
        file: ENGINE,
        find: "FillOrder::NewestFirst => count - 1 - position,",
        replace: "FillOrder::NewestFirst => position,",
        detector: Detector::Tests,
    },
    Canary {
        name: "no_anchor",
        meaning: "`startPosition` is ignored, so a missed fill stays missed",
        file: ENGINE,
        find: "Some(start) if fill.time_ms > market.fill_time_last_ms => start,",
        replace: "Some(start) if fill.time_ms == 0 => start,",
        detector: Detector::Tests,
    },
    Canary {
        name: "trust_stale_read",
        meaning: "a position read is believed though a newer fill may be missing from it",
        file: ENGINE,
        find: "!moving && time_ms > newest_fill_ms.saturating_add(u64::from(self.config.settle_margin_ms))",
        replace: "!moving && time_ms > newest_fill_ms",
        detector: Detector::Vopr(400),
    },
    Canary {
        name: "recovery_overwrites_fills",
        meaning: "recovery replaces a position that a newer fill already corrected",
        file: ENGINE,
        find: "let past_fills = time_ms > newest_fill_ms.saturating_add(margin_ms);\n            if self.tracks_position(market_index) && past_fills {\n                self.market_mut(market_index).position = reported",
        replace: "let past_fills = time_ms > newest_fill_ms.saturating_add(margin_ms) || true;\n            if self.tracks_position(market_index) && past_fills {\n                self.market_mut(market_index).position = reported",
        detector: Detector::Vopr(4000),
    },
    Canary {
        name: "reads_at_one_moment",
        meaning: "recovery reads positions at once, so an order between the reads is in neither",
        file: ENGINE,
        find: "self.positions_not_before_ms = self.now_ms + u64::from(self.config.settle_margin_ms);",
        replace: "self.positions_not_before_ms = self.now_ms;",
        detector: Detector::Tests,
    },
    Canary {
        name: "double_credit_adopted",
        meaning: "fills already inside an adopted order's remainder reduce it again",
        file: ENGINE,
        find: "credit_floor_ms: self.now_ms + u64::from(self.config.settle_margin_ms),",
        replace: "credit_floor_ms: 0,",
        detector: Detector::Vopr(4000),
    },
    Canary {
        name: "reduce_only_not_priced",
        meaning: "a reduce-only order removes the hedge and the gate does not see it",
        file: ENGINE,
        find: "position.saturating_sub(working.sells_reducing).max(position.min(0));",
        replace: "position;",
        detector: Detector::Tests,
    },
    Canary {
        name: "free_settling_on_timer",
        meaning: "a finished order's unseen fills leave the bound on a timer",
        file: ENGINE,
        find: "        slot.fills_retries = slot.fills_retries.saturating_add(1);\n",
        replace: "        slot.sz_final = slot.sz_filled;\n        slot.fills_retries = slot.fills_retries.saturating_add(1);\n",
        detector: Detector::Tests,
    },
    Canary {
        name: "cancel_not_sticky",
        meaning: "a refused cancel gives up, and an order landing after it lives on",
        file: ENGINE,
        find: "            slot.cancel = CancelState::Wanted;\n            slot.cancel_not_before_ms = self.now_ms + (poll_ms << doublings);",
        replace: "            slot.cancel = CancelState::None;\n            slot.cancel_not_before_ms = self.now_ms + (poll_ms << doublings);",
        detector: Detector::Tests,
    },
    Canary {
        name: "slot_generation_ignored",
        meaning: "a late reply lands on the different order now in its slot",
        file: ENGINE,
        find: "if held.state != State::Free && held.generation == slot.generation {",
        replace: "if held.state != State::Free {",
        detector: Detector::Tests,
    },
    Canary {
        name: "unknown_status_is_dead",
        meaning: "a status word nobody has read frees exposure",
        file: "crates/hypertiger/src/feed.rs",
        find: "            _ => StatusClass::Unknown,\n",
        replace: "            _ => StatusClass::Dead,\n",
        detector: Detector::Tests,
    },
    Canary {
        name: "adopt_zero_size",
        meaning: "a position TP/SL with no size is adopted, and an ordinary one crashes every start",
        file: ENGINE,
        find: "if order.sz_orig == Sz::ZERO || (order.cloid.is_none() && order.oid == 0) {",
        replace: "if order.cloid.is_none() && order.oid == 0 {",
        detector: Detector::Tests,
    },
    Canary {
        name: "unreadable_orders_are_none",
        meaning: "an error where the order list belongs reads as no orders, and recovery finishes",
        file: ENGINE,
        find: "if !self.open_orders_readable(value) {",
        replace: "if false && !self.open_orders_readable(value) {",
        detector: Detector::Tests,
    },
    Canary {
        name: "unreadable_position_is_flat",
        meaning: "a position entry that fails to parse reads as flat and drops out of the bound",
        file: ENGINE,
        find: "let position = crate::feed::Position::parse(item)?;",
        replace: "let Some(position) = crate::feed::Position::parse(item) else { continue };",
        detector: Detector::Tests,
    },
    Canary {
        name: "statuses_any_type",
        meaning: "an object where a status list belongs strands a slot with no request",
        file: ENGINE,
        find: "if statuses.kind() == Kind::Array && statuses.count() as usize == slots.len() =>",
        replace: "if statuses.count() as usize == slots.len() =>",
        detector: Detector::Tests,
    },
    Canary {
        name: "fill_from_the_future",
        meaning: "a fill stamped in the future freezes position tracking",
        file: ENGINE,
        find: "if fill.time_ms > self.now_ms.saturating_add(FILL_AHEAD_MS_MAX) {",
        replace: "if fill.time_ms > u64::MAX - 1 || FILL_AHEAD_MS_MAX == 0 {",
        detector: Detector::Tests,
    },
    Canary {
        name: "filled_means_all_of_it",
        meaning: "`filled` with a remainder is reported as fully filled",
        file: ENGINE,
        find: "self.mark_settling(index, order.sz_orig.saturating_sub(order.sz));\n                }\n            }\n            StatusClass::Dead",
        replace: "self.mark_settling(index, order.sz_orig);\n                }\n            }\n            StatusClass::Dead",
        detector: Detector::Tests,
    },
    Canary {
        name: "cloid_reuse_allowed",
        meaning: "a finished client id can go out again",
        file: ENGINE,
        find: "let finished_before = self.cloids_seen.contains(&Some(intent.cloid));",
        replace: "let finished_before = false && self.cloids_seen.contains(&Some(intent.cloid));",
        detector: Detector::Tests,
    },
    Canary {
        name: "clock_never_checked",
        meaning: "a clock the exchange disagrees with keeps trading on deadlines it cannot judge",
        file: ENGINE,
        find: "        if behind || ahead {\n",
        replace: "        if false && (behind || ahead) {\n",
        detector: Detector::Tests,
    },
    Canary {
        name: "full_budget_at_start",
        meaning: "every restart gets a full bucket, so a crash loop can pass the exchange's limit",
        file: ENGINE,
        find: "bucket: TokenBucket::new_empty(",
        replace: "bucket: TokenBucket::new(",
        detector: Detector::Vopr(1500),
    },
    Canary {
        name: "nonce_from_clock_alone",
        meaning: "nonces follow the clock, so a backward step repeats one",
        file: "crates/hypertiger/src/nonce.rs",
        find: "let nonce = now_ms.max(self.previous + 1);",
        replace: "let nonce = now_ms;",
        detector: Detector::Tests,
    },
    Canary {
        name: "bucket_burst_ignored",
        meaning: "the budget is rate alone, so rate plus burst can pass the exchange's limit",
        file: "crates/hypertiger/src/limits.rs",
        find: "        if self.credits >= wanted {\n",
        replace: "        let _ = wanted;\n        if true {\n",
        detector: Detector::Tests,
    },
    Canary {
        name: "order_fields_swapped",
        meaning: "field order changes, so the signature recovers a stranger",
        file: "crates/hypertiger/src/action.rs",
        find: "        sink.key(\"p\");\n        sink.str(self.px.to_wire().as_str());\n        sink.key(\"s\");\n        sink.str(self.sz.to_wire().as_str());\n",
        replace: "        sink.key(\"s\");\n        sink.str(self.sz.to_wire().as_str());\n        sink.key(\"p\");\n        sink.str(self.px.to_wire().as_str());\n",
        detector: Detector::Tests,
    },
    Canary {
        name: "expiry_unhashed",
        meaning: "the body says one expiry and the signature covers none",
        file: "crates/hypertiger/src/action.rs",
        find: "let hash = l1_action_hash(action, options.nonce, options.vault, options.expires_after_ms);",
        replace: "let hash = l1_action_hash(action, options.nonce, options.vault, None);",
        detector: Detector::Vopr(10),
    },
    Canary {
        name: "trailing_zeros_kept",
        meaning: "numbers keep trailing zeros, which is the docs' third signing trap",
        file: "crates/hypertiger/src/decimal.rs",
        find: "        while skip < decimals_count && skip < digits_count && digits[skip] == b'0' {\n            skip += 1;\n        }\n",
        replace: "",
        detector: Detector::Tests,
    },
    Canary {
        name: "tick_rule_loosened",
        meaning: "an off-tick price passes the check and costs a round trip to refuse",
        file: "crates/hypertiger/src/market.rs",
        find: "tick_decimals.max(tick_significant).min(WIRE_ONE)",
        replace: "tick_decimals.min(WIRE_ONE)",
        detector: Detector::Tests,
    },
    Canary {
        name: "json_trailing_garbage",
        meaning: "bytes after a document are accepted, so a torn stream can parse",
        file: "crates/hypertiger/src/json.rs",
        find: "            Expect::End => self.malformed(),\n",
        replace: "            Expect::End => {\n                self.cursor += 1;\n                Ok(Expect::End)\n            }\n",
        detector: Detector::Tests,
    },
    Canary {
        name: "ws_masked_server_frame",
        meaning: "a masked server frame is read as if it were not",
        file: "crates/hypertiger/src/ws.rs",
        find: "    if second & 0x80 != 0 {\n        return Err(WsError::Protocol);\n    }\n",
        replace: "",
        detector: Detector::Tests,
    },
];

pub fn run(root: &Path, arguments: &[String]) -> Result<(), String> {
    let only = arguments
        .iter()
        .position(|argument| argument == "--only")
        .and_then(|position| arguments.get(position + 1));
    preflight(root)?;
    let copy = root.join("target/canary-tree");
    prepare_copy(root, &copy)?;
    let mut survivors = Vec::new();
    let mut ran = 0u32;
    for canary in CANARIES {
        if only.is_some_and(|name| name != canary.name) {
            continue;
        }
        ran += 1;
        let path = copy.join(canary.file);
        let original = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        if !original.contains(canary.find) {
            return Err(format!(
                "{}: its target text is no longer in {}",
                canary.name, canary.file
            ));
        }
        let planted = original.replacen(canary.find, canary.replace, 1);
        std::fs::write(&path, &planted).map_err(|error| error.to_string())?;
        let caught = detect(&copy, canary.detector);
        std::fs::write(&path, &original).map_err(|error| error.to_string())?;
        let (word, seeds) = match canary.detector {
            Detector::Tests => ("tests", 0),
            Detector::Vopr(seeds) => ("vopr", seeds),
        };
        if caught? {
            println!("  caught   {:<26} by {word} {seeds}", canary.name);
        } else {
            println!("  SURVIVED {:<26} {}", canary.name, canary.meaning);
            survivors.push(canary.name);
        }
    }
    if ran == 0 {
        return Err("no canary ran".to_string());
    }
    if survivors.is_empty() {
        println!("  {ran} of {ran} planted bugs caught");
        Ok(())
    } else {
        Err(format!("planted bugs survived: {}", survivors.join(", ")))
    }
}

/// Checks every canary's target before any of them runs, because a full run takes a long while and a
/// stale target at the end of it would waste all of that.
fn preflight(root: &Path) -> Result<(), String> {
    let mut stale = Vec::new();
    for canary in CANARIES {
        let text = std::fs::read_to_string(root.join(canary.file)).map_err(|e| e.to_string())?;
        if !text.contains(canary.find) {
            stale.push(canary.name);
        }
    }
    if stale.is_empty() {
        println!("  {} canaries, every target present", CANARIES.len());
        Ok(())
    } else {
        Err(format!("these canaries' target text has moved: {}", stale.join(", ")))
    }
}

fn prepare_copy(root: &Path, copy: &Path) -> Result<(), String> {
    std::fs::create_dir_all(copy).map_err(|error| error.to_string())?;
    let status = Command::new("rsync")
        .args([
            "-a",
            "--delete",
            "--exclude",
            "target",
            "--exclude",
            ".git",
            "--exclude",
            "mutants.out*",
        ])
        .arg(format!("{}/", root.display()))
        .arg(format!("{}/", copy.display()))
        .status()
        .map_err(|error| format!("rsync: {error}"))?;
    if status.success() { Ok(()) } else { Err("rsync failed".to_string()) }
}

/// Whether the detector fails on the planted tree. A build failure is an error and not a catch: a
/// canary that does not compile has tested nothing.
fn detect(copy: &Path, detector: Detector) -> Result<bool, String> {
    let target = copy.join("target");
    let cargo = |arguments: &[&str]| {
        Command::new("nice")
            .args(["-n", "19", "cargo"])
            .args(arguments)
            .current_dir(copy)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_TERM_QUIET", "true")
            .output()
            .map_err(|error| format!("cargo: {error}"))
    };
    match detector {
        Detector::Tests => {
            let built = cargo(&["test", "-p", "hypertiger", "--no-run", "-j", crate::JOBS])?;
            if !built.status.success() {
                return Err(format!(
                    "a canary did not compile:\n{}",
                    String::from_utf8_lossy(&built.stderr)
                ));
            }
            Ok(!cargo(&["test", "-p", "hypertiger", "-j", crate::JOBS])?.status.success())
        }
        Detector::Vopr(seeds) => {
            let built = cargo(&["build", "--release", "-p", "hypertiger-vopr", "-j", crate::JOBS])?;
            if !built.status.success() {
                return Err(format!(
                    "a canary did not compile:\n{}",
                    String::from_utf8_lossy(&built.stderr)
                ));
            }
            let output = Command::new("nice")
                .args(["-n", "19"])
                .arg(target.join("release/hypertiger-vopr"))
                .args(["--seeds", &seeds.to_string(), "--jobs", crate::JOBS, "--no-coverage-gate"])
                .current_dir(copy)
                .output()
                .map_err(|error| format!("vopr: {error}"))?;
            Ok(!output.status.success())
        }
    }
}
