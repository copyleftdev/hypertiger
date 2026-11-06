//! The style rules a compiler will not check for us: line length, assertion density, recursion,
//! floats and allocation. Each rule is TigerStyle's, and `docs/STYLE.md` says how it maps to Rust.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// TigerStyle's number, held by the stateful core: the modules where an assertion states an invariant
/// of the program's own state. See `docs/STYLE.md` for why the reader and codec modules are not in
/// this list, and what pins them instead.
const DENSITY_MIN_HUNDREDTHS: u64 = 200;
const DENSITY_CORE: [&str; 5] = ["engine.rs", "nonce.rs", "limits.rs", "market.rs", "book.rs"];
/// A floor under the whole kernel, so that the part outside the core cannot quietly erode. It only
/// moves up.
const DENSITY_KERNEL_FLOOR_HUNDREDTHS: u64 = 80;
const LINE_WIDTH_MAX: usize = 100;

/// Source with the test module removed, which is everything a rule applies to.
struct Unit {
    name: String,
    code: String,
}

fn read_units(directory: &Path) -> Result<Vec<Unit>, String> {
    let mut units = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect();
    entries.sort();
    for path in entries {
        let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let code = text.split("#[cfg(test)]\nmod tests").next().unwrap_or_default().to_string();
        let name = path.file_name().map(|name| name.to_string_lossy().to_string());
        units.push(Unit { name: name.unwrap_or_default(), code });
    }
    Ok(units)
}

pub fn check(root: &Path) -> Result<(), String> {
    crate::step("style: limits a compiler does not check");
    let kernel = read_units(&root.join("crates/hypertiger/src"))?;
    let edge = read_units(&root.join("crates/hypertiger-net/src"))?;
    check_line_width(&kernel)?;
    check_line_width(&edge)?;
    check_density(&kernel)?;
    check_forbidden_tokens(&kernel)?;
    check_recursion(&kernel)?;
    Ok(())
}

fn check_line_width(units: &[Unit]) -> Result<(), String> {
    let mut lines_total = 0u64;
    for unit in units {
        for (index, line) in unit.code.lines().enumerate() {
            lines_total += 1;
            if line.chars().count() > LINE_WIDTH_MAX {
                let at = index + 1;
                return Err(format!("{}:{at} passes {LINE_WIDTH_MAX} columns", unit.name));
            }
        }
    }
    println!("  {lines_total} lines within {LINE_WIDTH_MAX} columns");
    Ok(())
}

/// Counts `fn` items with a body, and every assertion, compile-time ones included as TigerStyle
/// counts them. The runtime figure is printed too, so that it is visible what each kind carries.
fn check_density(units: &[Unit]) -> Result<(), String> {
    let mut core = Density::default();
    let mut kernel = Density::default();
    for unit in units {
        let density = Density::of(&strip_comments(&unit.code));
        kernel.add(&density);
        if DENSITY_CORE.contains(&unit.name.as_str()) {
            core.add(&density);
        }
        println!("    {:<12} {}", unit.name, density.describe());
    }
    println!("  core   {}", core.describe());
    println!("  kernel {}", kernel.describe());
    if core.hundredths() < DENSITY_MIN_HUNDREDTHS {
        return Err(format!("core assertion density {} is below 2", core.hundredths()));
    }
    if kernel.hundredths() < DENSITY_KERNEL_FLOOR_HUNDREDTHS {
        return Err(format!(
            "kernel assertion density {} fell below its floor",
            kernel.hundredths()
        ));
    }
    Ok(())
}

#[derive(Default)]
struct Density {
    functions: u64,
    runtime: u64,
    layout: u64,
}

impl Density {
    fn of(code: &str) -> Density {
        let layout = count_occurrences(code, "const _: () = assert!(");
        let total = count_occurrences(code, "assert!(")
            + count_occurrences(code, "assert_eq!(")
            + count_occurrences(code, "assert_ne!(");
        Density { functions: count_functions(code), runtime: total - layout, layout }
    }

    fn add(&mut self, other: &Density) {
        self.functions += other.functions;
        self.runtime += other.runtime;
        self.layout += other.layout;
    }

    fn hundredths(&self) -> u64 {
        (self.runtime + self.layout) * 100 / self.functions.max(1)
    }

    fn describe(&self) -> String {
        let density = self.hundredths();
        format!(
            "{}.{:02} per function ({} functions, {} at runtime, {} at compile time)",
            density / 100,
            density % 100,
            self.functions,
            self.runtime,
            self.layout
        )
    }
}

fn strip_comments(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    for line in code.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn count_occurrences(text: &str, needle: &str) -> u64 {
    text.matches(needle).count() as u64
}

fn count_functions(code: &str) -> u64 {
    let mut count = 0u64;
    let bytes = code.as_bytes();
    let mut search = 0usize;
    while let Some(found) = code[search..].find("fn ") {
        let at = search + found;
        search = at + 3;
        let boundary = at == 0 || !bytes[at - 1].is_ascii_alphanumeric() && bytes[at - 1] != b'_';
        if boundary && body_follows(&code[search..]) {
            count += 1;
        }
    }
    count
}

/// Whether a signature ends in a body rather than a semicolon, as a trait's declarations do.
fn body_follows(after_name: &str) -> bool {
    let mut depth = 0i32;
    for character in after_name.chars() {
        match character {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            '{' if depth == 0 => return true,
            ';' if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

/// Floats would break the exactness promise, and `alloc` the static-allocation one. `no_std` already
/// keeps `alloc` out of reach, and this makes reaching for it an obvious, named failure.
fn check_forbidden_tokens(units: &[Unit]) -> Result<(), String> {
    for unit in units {
        let code = strip_comments(&unit.code);
        for token in
            [" f32", " f64", "as f32", "as f64", "extern crate alloc", "std::", "Vec<", "Box<"]
        {
            // `core::` paths and the one `cfg(test)` std import are the only near misses, and the
            // test module is already gone by this point.
            if let Some(at) = code.find(token) {
                let line = code[..at].matches('\n').count() + 1;
                if !(token == "std::" && code[at.saturating_sub(5)..at].ends_with("core")) {
                    return Err(format!(
                        "{}: `{token}` in the kernel, near line {line}",
                        unit.name
                    ));
                }
            }
        }
    }
    println!("  no floats, no std, no alloc in the kernel");
    Ok(())
}

/// Finds call cycles from source text. It resolves `self.name(` and `Type::name(` to the enclosing
/// or named type and a bare `name(` to a free function; a call through a generic receiver such as
/// `action.walk(sink)` cannot be resolved this way. That one case is bounded separately, since
/// `Sink` panics past `NESTING_MAX`. This is a lint, not a proof, and `docs/STYLE.md` says so.
fn check_recursion(units: &[Unit]) -> Result<(), String> {
    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for unit in units {
        let module = unit.name.trim_end_matches(".rs");
        collect_calls(module, &strip_comments(&unit.code), &mut graph);
    }
    let known: BTreeSet<String> = graph.keys().cloned().collect();
    for callees in graph.values_mut() {
        callees.retain(|callee| known.contains(callee));
    }
    if let Some(cycle) = find_cycle(&graph) {
        return Err(format!("recursion: {}", cycle.join(" -> ")));
    }
    println!("  no call cycles among {} functions", graph.len());
    Ok(())
}

fn collect_calls(module: &str, code: &str, graph: &mut BTreeMap<String, BTreeSet<String>>) {
    let mut current_type = String::new();
    let mut current_fn: Option<String> = None;
    for line in code.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("impl") {
            current_type = impl_type(rest);
        } else if line.starts_with('}') {
            current_type.clear();
            current_fn = None;
        }
        if let Some(name) = fn_name(trimmed) {
            let scope = if current_type.is_empty() { module } else { &current_type };
            let qualified = format!("{scope}::{name}");
            graph.entry(qualified.clone()).or_default();
            current_fn = Some(qualified);
        } else if let Some(caller) = &current_fn {
            let scope = if current_type.is_empty() { module } else { &current_type };
            for callee in calls_in(trimmed, scope, module) {
                graph.entry(caller.clone()).or_default().insert(callee);
            }
        }
    }
}

fn impl_type(rest: &str) -> String {
    let header = rest.split('{').next().unwrap_or_default();
    let target = header.rsplit(" for ").next().unwrap_or_default().trim();
    let target = target.rsplit('>').next().filter(|tail| !tail.trim().is_empty()).unwrap_or(target);
    let name: String = target
        .trim()
        .trim_start_matches('<')
        .chars()
        .skip_while(|character| !character.is_alphabetic())
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    name
}

fn fn_name(trimmed: &str) -> Option<String> {
    let at = trimmed.find("fn ")?;
    let prefix = &trimmed[..at];
    let valid = prefix.is_empty()
        || prefix.trim_end().ends_with("pub")
        || prefix.trim_end().ends_with("const")
        || prefix.trim_end().ends_with(')');
    if !valid {
        return None;
    }
    let name: String = trimmed[at + 3..]
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

fn calls_in(line: &str, scope: &str, module: &str) -> Vec<String> {
    let mut calls = Vec::new();
    let bytes = line.as_bytes();
    for (open, _) in line.match_indices('(') {
        let mut start = open;
        while start > 0 {
            let byte = bytes[start - 1];
            if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b':' || byte == b'.' {
                start -= 1;
            } else {
                break;
            }
        }
        let path = &line[start..open];
        if let Some(name) = path.strip_prefix("self.") {
            if !name.contains('.') {
                calls.push(format!("{scope}::{name}"));
            }
        } else if let Some(name) = path.strip_prefix("Self::") {
            calls.push(format!("{scope}::{name}"));
        } else if let Some((owner, name)) = path.rsplit_once("::") {
            let owner = owner.rsplit("::").next().unwrap_or_default();
            calls.push(format!("{owner}::{name}"));
        } else if !path.is_empty() && !path.contains('.') {
            calls.push(format!("{module}::{path}"));
        }
    }
    calls
}

fn find_cycle(graph: &BTreeMap<String, BTreeSet<String>>) -> Option<Vec<String>> {
    // Iterative depth-first search: this checker holds itself to the rule it enforces.
    let mut finished: BTreeSet<&String> = BTreeSet::new();
    for start in graph.keys() {
        let mut path: Vec<&String> = Vec::new();
        let mut on_path: BTreeSet<&String> = BTreeSet::new();
        let mut stack: Vec<(&String, std::collections::btree_set::Iter<'_, String>)> = Vec::new();
        if finished.contains(start) {
            continue;
        }
        stack.push((start, graph[start].iter()));
        path.push(start);
        on_path.insert(start);
        while let Some((node, callees)) = stack.last_mut() {
            let node: &String = node;
            match callees.next() {
                Some(callee) if on_path.contains(callee) => {
                    let mut cycle: Vec<String> = path.iter().map(|item| (*item).clone()).collect();
                    cycle.push(callee.clone());
                    return Some(cycle);
                }
                Some(callee) if finished.contains(callee) => {}
                Some(callee) => {
                    if let Some(next) = graph.get(callee) {
                        stack.push((callee, next.iter()));
                        path.push(callee);
                        on_path.insert(callee);
                    }
                }
                None => {
                    finished.insert(node);
                    on_path.remove(node);
                    path.pop();
                    stack.pop();
                }
            }
        }
    }
    None
}
