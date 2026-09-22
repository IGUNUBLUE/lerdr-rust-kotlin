//! `lerdr-shadow diff` — compare two traces.
//!
//! Bucketing rules (see `docs/05-roadmap.md` Phase-3): frames whose
//! `request_id` names a `send` step — or whose `type` is in the tagged
//! step's `capture` list — form that step's *ordered* bucket. Everything
//! else lands in the *async pool*, a sorted multiset covering the startup
//! burst and unsolicited publishes whose interleaving is scheduler-owned.
//!
//! Both buckets compare post-normalization. The unified diff is rendered by
//! `similar`; the type census makes "side emits N extra frames" readable at
//! a glance even when every frame matches.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::normalize::{canonical, Normalizer};
use crate::scenario::CompareConfig;
use crate::trace::{Record, TraceFile};
use crate::Result;

/// One relay's trace reduced to comparable buckets.
struct Side {
    /// label → ordered normalized frames attributed to that step.
    step_buckets: BTreeMap<String, Vec<String>>,
    /// Unattributed normalized frames — the async pool.
    pool: Vec<String>,
    /// type → count over all rx frames *before* drop filtering.
    census: BTreeMap<String, usize>,
    notes: Vec<String>,
}

struct StepMarker {
    index: usize,
    label: String,
    request_id: Option<String>,
    capture: BTreeSet<String>,
}

pub struct DiffReport {
    /// True when every bucket and the async pool match.
    pub identical: bool,
    /// The full human-readable report (census + unified diffs).
    pub text: String,
}

/// Compare the traces at `a`/`b`. `config` overrides the compare section
/// embedded in side A's meta (used by the driver to test normalization
/// changes without re-running the relays).
pub fn diff_traces(a: &Path, b: &Path, config: Option<&CompareConfig>) -> Result<DiffReport> {
    let ta = TraceFile::load(a)?;
    let tb = TraceFile::load(b)?;
    let default_cfg = CompareConfig::default();
    let cfg = config
        .or_else(|| ta.compare_config())
        .or_else(|| tb.compare_config())
        .unwrap_or(&default_cfg);

    let mut out = String::new();
    let _ = writeln!(out, "=== shadow diff ===");
    let _ = writeln!(out, "a: {} ({})", ta.side(), a.display());
    let _ = writeln!(out, "b: {} ({})", tb.side(), b.display());
    for note in &cfg.notes {
        let _ = writeln!(out, "note: {note}");
    }
    out.push('\n');

    let normalizer = Normalizer::new(cfg);
    let sa = attribute(&ta, &normalizer, cfg);
    let sb = attribute(&tb, &normalizer, cfg);

    let mut identical = true;

    // -- type census ----------------------------------------------------------
    let mut types: BTreeSet<String> = sa.census.keys().cloned().collect();
    types.extend(sb.census.keys().cloned());
    let _ = writeln!(out, "type census (all rx frames, pre-drop):");
    let _ = writeln!(out, "  {:<28} {:>6} {:>6}", "type", "a", "b");
    for ty in &types {
        let ca = sa.census.get(ty).copied().unwrap_or(0);
        let cb = sb.census.get(ty).copied().unwrap_or(0);
        let flag = if ca != cb { "   ◂ differs" } else { "" };
        let _ = writeln!(out, "  {ty:<28} {ca:>6} {cb:>6}{flag}");
    }
    out.push('\n');

    // -- step buckets -----------------------------------------------------------
    let mut labels: BTreeSet<String> = sa.step_buckets.keys().cloned().collect();
    labels.extend(sb.step_buckets.keys().cloned());
    let _ = writeln!(out, "step buckets (ordered, normalized):");
    for label in &labels {
        let empty = Vec::new();
        let fa = sa.step_buckets.get(label).unwrap_or(&empty);
        let fb = sb.step_buckets.get(label).unwrap_or(&empty);
        if fa == fb {
            let _ = writeln!(out, "  {label}: {} frames — identical", fa.len());
            continue;
        }
        identical = false;
        let _ = writeln!(
            out,
            "  {label}: a:{} frames b:{} frames — DIFF",
            fa.len(),
            fb.len()
        );
        out.push_str(&unified(
            &format!("a/{label}"),
            &format!("b/{label}"),
            fa,
            fb,
        ));
    }
    if labels.is_empty() {
        let _ = writeln!(out, "  (no step buckets)");
    }
    out.push('\n');

    // -- async pool -------------------------------------------------------------
    let _ = writeln!(
        out,
        "async pool (sorted multiset): a={} b={} frames",
        sa.pool.len(),
        sb.pool.len()
    );
    if sa.pool == sb.pool {
        let _ = writeln!(out, "  identical");
    } else {
        identical = false;
        out.push_str(&unified("a/async", "b/async", &sa.pool, &sb.pool));
    }
    for note in &sa.notes {
        let _ = writeln!(out, "a note: {note}");
    }
    for note in &sb.notes {
        let _ = writeln!(out, "b note: {note}");
    }
    out.push('\n');
    let _ = writeln!(
        out,
        "RESULT: {}",
        if identical { "IDENTICAL" } else { "DIFFER" }
    );
    Ok(DiffReport {
        identical,
        text: out,
    })
}

/// Attribute a trace's `rx` frames to step buckets or the async pool.
fn attribute(trace: &TraceFile, normalizer: &Normalizer, cfg: &CompareConfig) -> Side {
    let mut steps: Vec<StepMarker> = Vec::new();
    for record in &trace.records {
        if let Record::Step {
            index,
            label,
            request_id,
            capture,
            ..
        } = record
        {
            steps.push(StepMarker {
                index: *index,
                label: label.clone(),
                request_id: request_id.clone(),
                capture: capture.iter().cloned().collect(),
            });
        }
    }
    let by_request: BTreeMap<&str, &StepMarker> = steps
        .iter()
        .filter_map(|s| s.request_id.as_deref().map(|id| (id, s)))
        .collect();
    let by_index: BTreeMap<usize, &StepMarker> = steps.iter().map(|s| (s.index, s)).collect();

    let mut step_buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut pool: Vec<String> = Vec::new();
    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    let mut notes: Vec<String> = Vec::new();

    for record in &trace.records {
        match record {
            Record::Rx { frame, step, .. } => {
                let ty = frame_type(frame);
                *census.entry(ty.clone()).or_insert(0) += 1;
                let Some(norm) = normalizer.frame(frame) else {
                    continue;
                };
                let line = canonical(&norm);
                // Rule 1: a captured type inside its step's window — wins
                // over unordered (the step asked for it explicitly).
                if let Some(marker) = step.and_then(|i| by_index.get(&i)) {
                    if marker.capture.contains(&ty) {
                        step_buckets
                            .entry(marker.label.clone())
                            .or_default()
                            .push(line);
                        continue;
                    }
                }
                // Rule 2: unordered types are scheduler-owned everywhere —
                // they pool even when they carry a request id, since their
                // relative order against the command_result is not
                // contractual.
                if cfg.unordered_types.contains(&ty) {
                    pool.push(line);
                    continue;
                }
                // Rule 3: a request id names its step even when the frame
                // arrived inside a later window.
                if let Some(req) = frame.get("request_id").and_then(Value::as_str) {
                    if let Some(marker) = by_request.get(req) {
                        step_buckets
                            .entry(marker.label.clone())
                            .or_default()
                            .push(line);
                        continue;
                    }
                }
                pool.push(line);
            }
            Record::Note { text, .. } => notes.push(text.clone()),
            _ => {}
        }
    }
    drop(by_request);
    drop(by_index);
    pool.sort();
    if cfg.async_dedupe {
        pool.dedup();
    }
    Side {
        step_buckets,
        pool,
        census,
        notes,
    }
}

fn frame_type(frame: &Value) -> String {
    frame
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("<untyped>")
        .to_owned()
}

/// Unified diff of two canonical-JSONL buckets via `similar`.
fn unified(a_name: &str, b_name: &str, a: &[String], b: &[String]) -> String {
    let a_text = a.join("\n");
    let b_text = b.join("\n");
    let diff = similar::TextDiff::from_lines(&a_text, &b_text);
    let mut out = String::new();
    let _ = write!(
        out,
        "{}",
        diff.unified_diff().context_radius(2).header(a_name, b_name)
    );
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::Step;
    use crate::trace::TraceWriter;
    use serde_json::json;

    fn write_trace(path: &Path, side: &str, compare: CompareConfig, records: Vec<Record>) {
        let mut w = TraceWriter::create(path).unwrap();
        w.write(&Record::Meta {
            side: side.into(),
            url: "ws://x".into(),
            scenario: "s".into(),
            started_ms: 0,
            compare,
        })
        .unwrap();
        for r in &records {
            w.write(r).unwrap();
        }
    }

    fn step(i: usize, label: &str, req: Option<&str>) -> Record {
        Record::Step {
            index: i,
            label: label.into(),
            op: "send".into(),
            request_id: req.map(str::to_owned),
            capture: vec![],
        }
    }

    fn rx(step: Option<usize>, frame: Value) -> Record {
        Record::Rx {
            t_ms: 0,
            step,
            frame,
        }
    }

    #[test]
    fn identical_traces_pass() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        let cfg = CompareConfig::default();
        let recs = vec![
            step(0, "s", Some("r0")),
            rx(
                Some(0),
                json!({"type": "command_result", "request_id": "r0"}),
            ),
            rx(None, json!({"type": "agents", "agents": []})),
        ];
        write_trace(&a, "a", cfg.clone(), recs.clone());
        write_trace(&b, "b", cfg, recs);
        let report = diff_traces(&a, &b, None).unwrap();
        assert!(report.identical, "{}", report.text);
    }

    #[test]
    fn async_pool_is_order_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        let cfg = CompareConfig::default();
        write_trace(
            &a,
            "a",
            cfg.clone(),
            vec![
                rx(None, json!({"type": "agents", "v": 1})),
                rx(None, json!({"type": "workspaces", "v": 2})),
            ],
        );
        write_trace(
            &b,
            "b",
            cfg,
            vec![
                rx(None, json!({"type": "workspaces", "v": 2})),
                rx(None, json!({"type": "agents", "v": 1})),
            ],
        );
        assert!(diff_traces(&a, &b, None).unwrap().identical);
    }

    #[test]
    fn extra_frame_diffs() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        write_trace(&a, "a", CompareConfig::default(), vec![]);
        write_trace(
            &b,
            "b",
            CompareConfig::default(),
            vec![rx(
                None,
                json!({"type": "action_receipt", "request_id": "r"}),
            )],
        );
        let report = diff_traces(&a, &b, None).unwrap();
        assert!(!report.identical);
        assert!(report.text.contains("action_receipt"));
    }

    #[test]
    fn capture_overrides_unordered() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        let mut cfg = CompareConfig::default();
        cfg.unordered_types.insert("push_policy".into());
        let recs = vec![
            Record::Step {
                index: 0,
                label: "pol".into(),
                op: "send".into(),
                request_id: Some("r0".into()),
                capture: vec!["push_policy".into()],
            },
            rx(Some(0), json!({"type": "push_policy", "policy": {"x": 1}})),
            // A second, unsolicited push_policy lands in the pool.
            rx(None, json!({"type": "push_policy", "policy": {"x": 2}})),
        ];
        write_trace(&a, "a", cfg.clone(), recs.clone());
        write_trace(&b, "b", cfg, recs);
        assert!(diff_traces(&a, &b, None).unwrap().identical);
        // Different captured value → diff lands in the step bucket.
        write_trace(
            &b,
            "b",
            CompareConfig::default(),
            vec![
                Record::Step {
                    index: 0,
                    label: "pol".into(),
                    op: "send".into(),
                    request_id: Some("r0".into()),
                    capture: vec!["push_policy".into()],
                },
                rx(Some(0), json!({"type": "push_policy", "policy": {"x": 9}})),
                rx(None, json!({"type": "push_policy", "policy": {"x": 2}})),
            ],
        );
        let cfg2 = {
            let mut c = CompareConfig::default();
            c.unordered_types.insert("push_policy".into());
            c
        };
        let report = diff_traces(&a, &b, Some(&cfg2)).unwrap();
        assert!(!report.identical);
    }

    #[test]
    fn unordered_beats_request_id() {
        // `activity` carries the triggering request_id on both relays but its
        // arrival order vs the command_result is scheduler-owned: it must
        // pool even though rule 3 would attribute it to the send step.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        let mut cfg = CompareConfig::default();
        cfg.unordered_types.insert("activity".into());
        let recs = vec![
            step(0, "s", Some("r0")),
            rx(Some(0), json!({"type": "activity", "request_id": "r0"})),
            rx(
                Some(0),
                json!({"type": "command_result", "request_id": "r0"}),
            ),
        ];
        write_trace(&a, "a", cfg.clone(), recs.clone());
        // b emits the activity before the command_result — same pool.
        let recs_b = vec![
            step(0, "s", Some("r0")),
            rx(
                Some(0),
                json!({"type": "command_result", "request_id": "r0"}),
            ),
            rx(Some(0), json!({"type": "activity", "request_id": "r0"})),
        ];
        write_trace(&b, "b", cfg, recs_b);
        let report = diff_traces(&a, &b, None).unwrap();
        assert!(report.identical, "{}", report.text);
    }

    #[test]
    fn request_id_attributes_across_steps() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jsonl");
        let b = dir.path().join("b.jsonl");
        // The response for step 0 arrives inside step 1's window on both
        // sides → attributed to step 0 by request id either way.
        let recs = vec![
            step(0, "first", Some("r0")),
            step(1, "second", Some("r1")),
            rx(
                Some(1),
                json!({"type": "command_result", "request_id": "r0"}),
            ),
            rx(
                Some(1),
                json!({"type": "command_result", "request_id": "r1"}),
            ),
        ];
        write_trace(&a, "a", CompareConfig::default(), recs.clone());
        write_trace(&b, "b", CompareConfig::default(), recs);
        assert!(diff_traces(&a, &b, None).unwrap().identical);
    }

    #[test]
    fn step_labels_unique_check() {
        // Scenario loading doesn't enforce it — the differ keys buckets by
        // label, so duplicates merge silently. Document that here.
        let s: crate::scenario::Scenario = serde_json::from_value(json!({
            "steps": [
                {"op": "settle", "label": "x", "ms": 1},
                {"op": "settle", "label": "x", "ms": 1}
            ]
        }))
        .unwrap();
        assert_eq!(s.steps.len(), 2);
        let _ = Step::Settle {
            label: "x".into(),
            ms: 1,
        };
    }
}
