//! Frame normalization — erase volatile values before comparison without
//! hiding semantic ones. Driven entirely by the scenario's `compare` block.
//!
//! - `drop_keys`: remove the key at any depth (timestamps, server sequence
//!   counters, `_server_*` transport metadata).
//! - `map_keys`: keep the key but replace the value with `"<mapped>"` —
//!   presence still compares, contents don't (random ids, proofs, paths).
//! - `drop_types`: skip whole frames by `type` (transport-level frames).

use serde_json::{Map, Value};
use std::collections::BTreeSet;

use crate::scenario::CompareConfig;

/// The marker substituted for `map_keys` values.
pub const MAPPED: &str = "<mapped>";

pub struct Normalizer<'a> {
    config: &'a CompareConfig,
}

impl<'a> Normalizer<'a> {
    pub fn new(config: &'a CompareConfig) -> Self {
        Self { config }
    }

    /// Normalize one decoded outbound frame; `None` means the type is dropped.
    pub fn frame(&self, frame: &Value) -> Option<Value> {
        let ty = frame.get("type").and_then(Value::as_str).unwrap_or("");
        if self.config.drop_types.contains(ty) {
            return None;
        }
        if self.config.drop_matches.iter().any(|m| m.is_match(frame)) {
            return None;
        }
        let typed_drops = self.config.type_drop_keys.get(ty);
        Some(self.value(frame, typed_drops))
    }

    fn value(&self, value: &Value, typed_drops: Option<&BTreeSet<String>>) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = Map::with_capacity(map.len());
                for (key, item) in map {
                    if self.config.drop_keys.contains(key)
                        || typed_drops.is_some_and(|set| set.contains(key))
                    {
                        continue;
                    }
                    if self.config.map_keys.contains(key) {
                        out.insert(key.clone(), Value::String(MAPPED.to_owned()));
                        continue;
                    }
                    out.insert(key.clone(), self.value(item, typed_drops));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| self.value(item, typed_drops))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

/// Canonical minified JSON — `serde_json::Map` is `BTreeMap`-backed in this
/// workspace (no `preserve_order` feature), so serialization is already
/// sorted-key order: a stable, diffable line form.
pub fn canonical(value: &Value) -> String {
    serde_json::to_string(value).expect("a decoded frame re-serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn cfg() -> CompareConfig {
        CompareConfig {
            drop_types: BTreeSet::from(["noise".to_owned()]),
            drop_keys: BTreeSet::from(["at".to_owned(), "_server_sequence".to_owned()]),
            map_keys: BTreeSet::from(["device_id".to_owned()]),
            ..CompareConfig::default()
        }
    }

    #[test]
    fn drops_and_maps_recursively() {
        let cfg = cfg();
        let n = Normalizer::new(&cfg);
        let frame = json!({
            "type": "activity",
            "activity": {"id": "a1", "at": 123, "device": {"device_id": "xyz"}},
            "_server_sequence": 9
        });
        let got = n.frame(&frame).unwrap();
        assert_eq!(
            got,
            json!({"type": "activity", "activity": {"id": "a1", "device": {"device_id": "<mapped>"}}})
        );
    }

    #[test]
    fn drop_type_returns_none() {
        let cfg = cfg();
        let n = Normalizer::new(&cfg);
        assert!(n.frame(&json!({"type": "noise", "x": 1})).is_none());
    }

    #[test]
    fn drop_matches_predicates() {
        use crate::scenario::Match;
        let mut c = cfg();
        c.drop_matches.push(Match {
            r#type: "activity_history".into(),
            request_id: None,
            contains: Some(json!({"activities": null})),
        });
        let n = Normalizer::new(&c);
        assert!(n
            .frame(&json!({"type": "activity_history", "activities": null}))
            .is_none());
        assert!(n
            .frame(&json!({"type": "activity_history", "activities": []}))
            .is_some());
        assert!(n.frame(&json!({"type": "agents", "agents": []})).is_some());
    }

    #[test]
    fn type_drop_keys_scoped_to_type() {
        let mut c = cfg();
        c.type_drop_keys
            .insert("agents".into(), BTreeSet::from(["project".to_owned()]));
        let n = Normalizer::new(&c);
        let got = n
            .frame(&json!({
                "type": "agents",
                "agents": [{"pane_id": "p1", "project": "x"}]
            }))
            .unwrap();
        assert_eq!(
            got,
            json!({"type": "agents", "agents": [{"pane_id": "p1"}]})
        );
        // Same key survives inside a different frame type.
        let got = n.frame(&json!({"type": "other", "project": "x"})).unwrap();
        assert_eq!(got, json!({"type": "other", "project": "x"}));
    }

    #[test]
    fn canonical_sorts_keys() {
        assert_eq!(
            canonical(&json!({"b": 1, "a": {"d": 4, "c": 3}})),
            r#"{"a":{"c":3,"d":4},"b":1}"#
        );
    }
}
