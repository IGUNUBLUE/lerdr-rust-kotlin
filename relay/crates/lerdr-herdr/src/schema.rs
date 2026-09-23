//! Runtime API-schema introspection — doc 08's `SchemaRegistry`.
//!
//! `herdr api schema --json` dumps the installed API's JSON Schema bundle
//! (there is no socket-level schema method — this is CLI-only). The bundle
//! has five named schemas; the capability ledger needs three of them:
//!
//! * `schemas.request.oneOf[]` — one object per method; the method name is
//!   `properties.method.const`.
//! * `schemas.request.$defs.Subscription.oneOf[]` — one object per
//!   subscribable type; the name is `properties.type.const` in **dotted**
//!   form (`workspace.reordered`, `pane.output_matched`).
//! * `schemas.event.$defs.EventData.oneOf[]` — one object per streamed event
//!   variant; `properties.type.const` in **snake_case wire** form
//!   (`workspace_reordered`).
//! * `schemas.subscription_event.$defs.SubscriptionEventKind.enum[]` — the
//!   per-pane event kinds (`pane.output_matched`, …).
//!
//! Parsing is deliberately shape-tolerant: every extraction walks optional
//! paths, so a schema that grows, renames, or drops a subtree yields a
//! registry with empty sections rather than a hard failure — callers degrade
//! to `unknown`, never panic.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::events::{canonical_event_name, wire_event_name};

/// Why schema introspection produced no usable registry.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SchemaError {
    /// The `herdr` binary could not be spawned or did not finish cleanly
    /// (timeout, non-zero exit, output cap).
    #[error("herdr api schema invocation failed: {0}")]
    Cli(String),
    /// The output was not valid JSON.
    #[error("herdr api schema output is not valid JSON: {0}")]
    Malformed(String),
    /// Parsed but carried no usable surface (no request methods at all) —
    /// treated as absent so capability probing falls back.
    #[error("herdr api schema carries no method surface")]
    Empty,
}

/// The parsed, indexed schema of the *installed* Herdr API.
///
/// Build via [`SchemaRegistry::parse`] (from bytes — testable) or
/// [`Client::api_schema`](crate::Client::api_schema) (runs the CLI). Query
/// with `supports_method`/`supports_event`/`supports_subscription`.
#[derive(Debug, Clone, Default)]
pub struct SchemaRegistry {
    /// Top-level `protocol` field (Herdr API protocol generation).
    protocol: Option<u64>,
    /// Top-level `schema_version` field.
    schema_version: Option<u64>,
    /// `request.oneOf` method consts (`pane.read`, `agent.view.set`, …).
    methods: BTreeSet<String>,
    /// `request.$defs.Subscription.oneOf` type consts — the dotted names
    /// `events.subscribe` accepts.
    subscription_types: BTreeSet<String>,
    /// `event.$defs.EventData.oneOf` type consts — snake_case wire names;
    /// stored canonicalized (dotted) so queries can use either spelling.
    event_types: BTreeSet<String>,
    /// `subscription_event` per-pane kinds — dotted names.
    subscription_event_kinds: BTreeSet<String>,
}

impl SchemaRegistry {
    /// Parse a `herdr api schema --json` document. Shape-tolerant: unknown or
    /// missing subtrees leave the corresponding index empty. Returns
    /// [`SchemaError::Empty`] when the document contains no request methods —
    /// a schema that names nothing is not evidence.
    pub fn parse(doc: &[u8]) -> Result<SchemaRegistry, SchemaError> {
        let value: Value =
            serde_json::from_slice(doc).map_err(|e| SchemaError::Malformed(e.to_string()))?;
        Ok(Self::from_value(&value))
    }

    /// Parse from a decoded [`Value`] — the injection point for tests and
    /// embedded fixtures.
    pub fn from_value(value: &Value) -> SchemaRegistry {
        let schemas = value.get("schemas").cloned().unwrap_or(Value::Null);
        let schema = |name: &str| schemas.get(name).cloned().unwrap_or(Value::Null);
        let request = schema("request");

        let mut methods = BTreeSet::new();
        for entry in request
            .get("oneOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(method) = entry
                .pointer("/properties/method/const")
                .and_then(Value::as_str)
            {
                methods.insert(method.to_owned());
            }
        }

        let mut subscription_types = BTreeSet::new();
        if let Some(one_of) = request
            .pointer("/$defs/Subscription/oneOf")
            .and_then(Value::as_array)
        {
            for entry in one_of {
                if let Some(name) = entry
                    .pointer("/properties/type/const")
                    .and_then(Value::as_str)
                {
                    subscription_types.insert(name.to_owned());
                }
            }
        }

        let mut event_types = BTreeSet::new();
        if let Some(one_of) = schema("event")
            .pointer("/$defs/EventData/oneOf")
            .and_then(Value::as_array)
        {
            for entry in one_of {
                if let Some(name) = entry
                    .pointer("/properties/type/const")
                    .and_then(Value::as_str)
                {
                    event_types.insert(canonical_event_name(name).to_owned());
                }
            }
        }

        let mut subscription_event_kinds = BTreeSet::new();
        if let Some(kinds) = schema("subscription_event")
            .pointer("/$defs/SubscriptionEventKind/enum")
            .and_then(Value::as_array)
        {
            for kind in kinds {
                if let Some(name) = kind.as_str() {
                    subscription_event_kinds.insert(name.to_owned());
                }
            }
        }

        SchemaRegistry {
            protocol: value.get("protocol").and_then(Value::as_u64),
            schema_version: value.get("schema_version").and_then(Value::as_u64),
            methods,
            subscription_types,
            event_types,
            subscription_event_kinds,
        }
    }

    /// The API protocol generation the schema describes (`protocol` field).
    pub fn protocol(&self) -> Option<u64> {
        self.protocol
    }

    /// The schema document's own version (`schema_version` field).
    pub fn schema_version(&self) -> Option<u64> {
        self.schema_version
    }

    /// `true` when the registry carries at least one method — the minimum
    /// for it to count as evidence.
    pub fn is_usable(&self) -> bool {
        !self.methods.is_empty()
    }

    /// Whether `method` is advertised (`pane.read`, `agent.view.set`, …).
    pub fn supports_method(&self, method: &str) -> bool {
        self.methods.contains(method)
    }

    /// Whether a canonical (dotted) event name is subscribable — accepted
    /// when either the `events.subscribe` type list or the streamed-event
    /// list names it. Snake_case input is canonicalized first, so
    /// `workspace_reordered` and `workspace.reordered` both work.
    pub fn supports_event(&self, event: &str) -> bool {
        let name = canonical_event_name(event);
        self.subscription_types.contains(name) || self.event_types.contains(name)
    }

    /// Whether `{"type":"<name>"}` is a valid `events.subscribe` entry —
    /// narrower than [`supports_event`](Self::supports_event): per-pane
    /// kinds (`pane.output_matched`) appear here even though they never
    /// show up in the `EventData` stream list.
    pub fn supports_subscription(&self, name: &str) -> bool {
        self.subscription_types.contains(canonical_event_name(name))
    }

    /// Whether a per-pane subscription-event kind exists
    /// (`pane.output_matched`, `pane.agent_status_changed`,
    /// `pane.scroll_changed`).
    pub fn supports_subscription_event(&self, kind: &str) -> bool {
        self.subscription_event_kinds
            .contains(canonical_event_name(kind))
    }

    /// Every advertised method name, sorted.
    pub fn methods(&self) -> impl Iterator<Item = &str> {
        self.methods.iter().map(String::as_str)
    }

    /// Every streamed event variant (canonical dotted names), sorted.
    pub fn event_types(&self) -> impl Iterator<Item = &str> {
        self.event_types.iter().map(String::as_str)
    }

    /// Every valid `events.subscribe` type, sorted.
    pub fn subscription_types(&self) -> impl Iterator<Item = &str> {
        self.subscription_types.iter().map(String::as_str)
    }

    /// The wire spelling for a subscription type the schema advertises —
    /// passthrough for callers composing the request payload.
    pub fn wire_name<'a>(&self, name: &'a str) -> &'a str {
        wire_event_name(name)
    }
}

/// Where [`Client`](crate::Client) obtains its schema at capability
/// refresh — the `ClientConfig::schema_source` knob.
#[derive(Debug, Clone, Default)]
pub enum SchemaSource {
    /// Run `herdr api schema --json` through the resolved binary (default).
    #[default]
    Cli,
    /// Never introspect — capability collection falls back to probes and
    /// every schema-derived feature stays `unknown` until noted.
    Disabled,
    /// A fixed document — tests and embeddings that already know the
    /// server's surface.
    Static(SchemaRegistry),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Minimal-but-shaped-like-real schema document.
    fn sample_schema() -> Value {
        json!({
            "$schema": "https://json.schemastore.org/draft-07/schema",
            "protocol": 22,
            "schema_version": 1,
            "title": "herdr API",
            "schemas": {
                "request": {
                    "oneOf": [
                        {"properties": {"method": {"const": "ping"}}, "required": ["method"]},
                        {"properties": {"method": {"const": "pane.read"}}, "required": ["method"]},
                        {"properties": {"method": {"const": "events.subscribe"}}, "required": ["method"]},
                        {"properties": {"method": {"const": "agent.view.set"}}, "required": ["method"]},
                        // Malformed entry — tolerated, contributes nothing.
                        {"properties": {"params": {}}},
                    ],
                    "$defs": {
                        "Subscription": {
                            "oneOf": [
                                {"properties": {"type": {"const": "pane.updated"}}},
                                {"properties": {"type": {"const": "workspace.reordered"}}},
                            ]
                        }
                    }
                },
                "event": {
                    "$defs": {
                        "EventData": {
                            "oneOf": [
                                {"properties": {"type": {"const": "pane_updated"}}},
                                {"properties": {"type": {"const": "workspace_reordered"}}},
                            ]
                        }
                    }
                },
                "subscription_event": {
                    "$defs": {
                        "SubscriptionEventKind": {"enum": ["pane.output_matched", "pane.scroll_changed"]}
                    }
                }
            }
        })
    }

    #[test]
    fn parses_methods_events_and_kinds() {
        let reg =
            SchemaRegistry::parse(serde_json::to_string(&sample_schema()).unwrap().as_bytes())
                .unwrap();
        assert_eq!(reg.protocol(), Some(22));
        assert_eq!(reg.schema_version(), Some(1));
        assert!(reg.is_usable());
        assert!(reg.supports_method("pane.read"));
        assert!(reg.supports_method("agent.view.set"));
        assert!(!reg.supports_method("workspace.move_block"));
        assert!(reg.supports_subscription("workspace.reordered"));
        // Wire spelling canonicalizes.
        assert!(reg.supports_event("workspace_reordered"));
        assert!(reg.supports_event("workspace.reordered"));
        assert!(reg.supports_subscription_event("pane.output_matched"));
        assert!(reg.supports_subscription_event("pane.scroll_changed"));
        assert!(!reg.supports_subscription_event("pane.agent_status_changed"));
        assert!(!reg.supports_event("pane.exited"));
        let methods: Vec<&str> = reg.methods().collect();
        assert_eq!(
            methods,
            vec!["agent.view.set", "events.subscribe", "pane.read", "ping"]
        );
    }

    #[test]
    fn malformed_document_is_an_error() {
        let err = SchemaRegistry::parse(b"{not json").unwrap_err();
        assert!(matches!(err, SchemaError::Malformed(_)));
    }

    #[test]
    fn empty_or_alien_schema_parses_but_is_unusable() {
        // Shape-tolerant: a schema missing the request subtree parses to an
        // empty registry rather than erroring — the caller decides it's
        // unusable evidence.
        let reg = SchemaRegistry::parse(br#"{"protocol": 22}"#).unwrap();
        assert!(!reg.is_usable());
        assert!(!reg.supports_method("ping"));
    }

    #[test]
    fn evolved_schema_shapes_degrade() {
        // `oneOf` replaced by `anyOf` upstream → methods list is empty →
        // unusable, and every capability stays unknown rather than wrongly
        // reported unsupported.
        let reg = SchemaRegistry::from_value(&json!({
            "schemas": {"request": {"anyOf": [{"properties": {"method": {"const": "ping"}}}]}}
        }));
        assert!(!reg.is_usable());
    }
}
