//! Engine bounds on captain-supplied stored values (engine 1.55): JSON nesting depth and size.
//!
//! The mcp layer refuses arguments nested deeper than its intake cap before anything is
//! journaled; this is the engine's own, lower bound on what a captain can store: structured
//! memory (save, patch, append, import and data sent as a JSON string), market claims and notes.
//! A value over a bound is refused whole (`too_deep`, `too_large`) and nothing is stored; nothing
//! is ever cut. The checks run in the command handlers, so they apply to new commands only:
//! records stored before 1.55 that are larger or deeper still load and read, and refusals are
//! never journaled, so replay is unchanged.
//!
//! Depth counts containers: a scalar is 0, `{}` and `[]` are 1, `[[1]]` is 2. Counted without
//! recursion, so no value can overflow the stack here.

use serde_json::Value;

use crate::engine::{Refusal, Res};

/// The deepest JSON nesting a captain may store (memory values, market claims).
pub const MAX_JSON_DEPTH: usize = 32;
/// Largest note, in bytes of UTF-8 text (the whole note after an append).
pub const NOTE_MAX_BYTES: usize = 160 * 1024;
/// Largest memory value, in bytes of its compact JSON (the whole value after a patch or append).
pub const MEMORY_VALUE_MAX_BYTES: usize = 256 * 1024;
/// Largest market claim, in bytes of its compact JSON.
pub const CLAIM_MAX_BYTES: usize = 16 * 1024;

/// Container nesting depth of a JSON value, counted without recursion.
pub fn depth(v: &Value) -> usize {
    let mut deepest = 0;
    let mut stack = vec![(v, 1usize)];
    while let Some((v, d)) = stack.pop() {
        match v {
            Value::Object(m) => {
                deepest = deepest.max(d);
                stack.extend(m.values().map(|c| (c, d + 1)));
            }
            Value::Array(a) => {
                deepest = deepest.max(d);
                stack.extend(a.iter().map(|c| (c, d + 1)));
            }
            _ => {}
        }
    }
    deepest
}

/// The maximum nesting of arrays and objects in a JSON text, counted without recursion and
/// outside strings (escaped quotes included). Invalid JSON gets a best-effort count.
pub fn text_depth(text: &str) -> usize {
    let (mut depth, mut max, mut in_str, mut escaped) = (0usize, 0usize, false, false);
    for b in text.bytes() {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Bytes of a value's compact JSON. Call only after [`check_depth`], so serialising is shallow.
pub fn json_bytes(v: &Value) -> usize {
    serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX)
}

fn too_deep(field: &str, depth: usize) -> Refusal {
    Refusal::new(
        "too_deep",
        format!("{field} is nested {depth} levels deep; at most {MAX_JSON_DEPTH}. Nothing was stored."),
    )
    .hint(format!(
        "Flatten it to at most {MAX_JSON_DEPTH} levels of objects and arrays (a scalar is 0, {{}} and [] are 1)."
    ))
}

/// Refuse a value nested deeper than [`MAX_JSON_DEPTH`].
pub fn check_depth(field: &str, v: &Value) -> Res<()> {
    let d = depth(v);
    if d > MAX_JSON_DEPTH {
        return Err(too_deep(field, d));
    }
    Ok(())
}

/// Refuse a JSON text nested deeper than [`MAX_JSON_DEPTH`], before it is parsed.
pub fn check_text_depth(field: &str, text: &str) -> Res<()> {
    let d = text_depth(text);
    if d > MAX_JSON_DEPTH {
        return Err(too_deep(field, d));
    }
    Ok(())
}

fn too_large(field: &str, bytes: usize, max: usize) -> Refusal {
    Refusal::new("too_large", format!("{field} is {bytes} bytes; at most {max}. Nothing was stored."))
        .hint("Split it across several keys, or keep a shorter summary.")
}

/// Refuse a value deeper than [`MAX_JSON_DEPTH`] or larger than `max_bytes` of compact JSON.
pub fn check_value(field: &str, v: &Value, max_bytes: usize) -> Res<()> {
    check_depth(field, v)?;
    let n = json_bytes(v);
    if n > max_bytes {
        return Err(too_large(field, n, max_bytes));
    }
    Ok(())
}

/// Refuse a text longer than `max_bytes` of UTF-8.
pub fn check_text(field: &str, text: &str, max_bytes: usize) -> Res<()> {
    if text.len() > max_bytes {
        return Err(too_large(field, text.len(), max_bytes));
    }
    Ok(())
}
