//! The schema compactor engine.
//!
//! Two related transforms live here:
//!
//! 1. [`compact_schema`] / [`compact_tool_signature`] — turn a JSON Schema into
//!    a dense TypeScript-style type signature. This is what the aggressive
//!    `high` tier emits, e.g.
//!        `(p:{jql:string,max_results?:number})=>any`
//!    Maximum token savings; the model reads the signature instead of a
//!    400-line JSON Schema.
//!
//! 2. [`compact_jsonschema`] — losslessly *structural* compaction that keeps a
//!    valid JSON Schema but strips metadata (`$schema`, `title`, `examples`,
//!    `description`, …). Powers the lower-risk `safe`/`balanced` tiers, where
//!    the host still enforces a real schema so no rehydration is required.
//!
//! Determinism note: `serde_json::Map` is a `BTreeMap` by default, so object
//! keys are emitted in sorted order. That makes signatures stable across runs
//! — useful for caching, hashing, and the tests below.

use serde_json::{Map, Value};

/// Render a tool's argument schema as a TS signature line:
/// `type <name> = (p:<type>)=>any;`
///
/// The tool name is used verbatim; MCP tool names aren't always valid TS
/// identifiers, but the signature is a hint for the model to read, not code we
/// compile, so legibility wins over strict syntax.
#[allow(dead_code)]
pub fn compact_tool_signature(name: &str, input_schema: &Value) -> String {
    format!("type {name} = (p:{})=>any;", compact_schema(input_schema))
}

/// Convert a JSON Schema node into a compact TS type string.
pub fn compact_schema(schema: &Value) -> String {
    ts_type(schema)
}

fn ts_type(schema: &Value) -> String {
    // `const` and `enum` become literal / union-of-literal types.
    if let Some(c) = schema.get("const") {
        return literal(c);
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.is_empty() {
            return values.iter().map(literal).collect::<Vec<_>>().join("|");
        }
    }

    // Combinators.
    for key in ["anyOf", "oneOf"] {
        if let Some(arr) = schema.get(key).and_then(Value::as_array) {
            if !arr.is_empty() {
                return arr.iter().map(ts_type).collect::<Vec<_>>().join("|");
            }
        }
    }
    if let Some(arr) = schema.get("allOf").and_then(Value::as_array) {
        if !arr.is_empty() {
            return arr.iter().map(ts_type).collect::<Vec<_>>().join("&");
        }
    }

    // The `type` keyword, which may be a string or an array of strings.
    match schema.get("type") {
        Some(Value::String(t)) => ts_from_type(t, schema),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .map(|t| ts_from_type(t, schema))
            .collect::<Vec<_>>()
            .join("|"),
        _ => {
            // No explicit type: infer from shape.
            if schema.get("properties").is_some() {
                object_type(schema)
            } else if schema.get("items").is_some() {
                array_type(schema)
            } else {
                "any".to_string()
            }
        }
    }
}

fn ts_from_type(t: &str, schema: &Value) -> String {
    match t {
        "string" => "string".to_string(),
        // TS has no integer type; both map to `number`.
        "number" | "integer" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => array_type(schema),
        "object" => object_type(schema),
        // Unknown type token: surface it rather than guess.
        other => other.to_string(),
    }
}

fn array_type(schema: &Value) -> String {
    match schema.get("items") {
        // Homogeneous array: `T[]`. Parenthesize unions so `(a|b)[]` parses right.
        Some(items @ Value::Object(_)) => {
            let inner = ts_type(items);
            if inner.contains('|') || inner.contains('&') {
                format!("({inner})[]")
            } else {
                format!("{inner}[]")
            }
        }
        // Tuple form: `[T1,T2]`.
        Some(Value::Array(tuple)) => {
            let parts: Vec<String> = tuple.iter().map(ts_type).collect();
            format!("[{}]", parts.join(","))
        }
        _ => "any[]".to_string(),
    }
}

fn object_type(schema: &Value) -> String {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut fields: Vec<String> = Vec::new();
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (key, sub) in props {
            let optional = if required.contains(&key.as_str()) { "" } else { "?" };
            fields.push(format!("{}{}:{}", format_key(key), optional, ts_type(sub)));
        }
    }

    if fields.is_empty() {
        // Open map / no declared properties.
        return match schema.get("additionalProperties") {
            Some(ap @ Value::Object(_)) => format!("Record<string,{}>", ts_type(ap)),
            Some(Value::Bool(false)) => "{}".to_string(),
            _ => "Record<string,any>".to_string(),
        };
    }

    format!("{{{}}}", fields.join(","))
}

/// Quote a property key if it isn't a bare TS identifier.
fn format_key(key: &str) -> String {
    let mut chars = key.chars();
    let valid_head = chars
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        .unwrap_or(false);
    let valid_tail = chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if valid_head && valid_tail {
        key.to_string()
    } else {
        format!("\"{}\"", key.replace('"', "\\\""))
    }
}

/// Render a JSON value as a TS literal type.
fn literal(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{}\"", s.replace('"', "\\\"")),
        Value::Null => "null".to_string(),
        // numbers and booleans render as themselves
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// JSON-Schema-preserving compaction (safe / balanced tiers)
// ---------------------------------------------------------------------------

/// Keys that are pure metadata: they cost tokens but don't constrain arguments,
/// so they're always safe to drop.
const METADATA_KEYS: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "title",
    "examples",
    "default",
    "readOnly",
    "writeOnly",
    "deprecated",
];

/// Produce a still-valid JSON Schema with metadata removed.
///
/// - `keep_descriptions = false` drops `description` entirely (the `safe` tier).
/// - `keep_descriptions = true` keeps descriptions truncated to `max_desc`
///   characters (the `balanced` tier) — descriptions drive correct tool
///   selection, so a short one is worth its tokens.
pub fn compact_jsonschema(schema: &Value, keep_descriptions: bool, max_desc: usize) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                if METADATA_KEYS.contains(&key.as_str()) {
                    continue;
                }
                if key == "description" {
                    if keep_descriptions {
                        if let Some(s) = value.as_str() {
                            out.insert(key.clone(), Value::String(truncate(s, max_desc)));
                        }
                    }
                    continue;
                }
                out.insert(key.clone(), compact_jsonschema(value, keep_descriptions, max_desc));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| compact_jsonschema(v, keep_descriptions, max_desc))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Character-safe truncation with an ellipsis marker.
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn primitives() {
        assert_eq!(compact_schema(&json!({"type":"string"})), "string");
        assert_eq!(compact_schema(&json!({"type":"integer"})), "number");
        assert_eq!(compact_schema(&json!({"type":"number"})), "number");
        assert_eq!(compact_schema(&json!({"type":"boolean"})), "boolean");
        assert_eq!(compact_schema(&json!({"type":"null"})), "null");
    }

    #[test]
    fn object_required_vs_optional() {
        let s = json!({
            "type":"object",
            "properties":{"jql":{"type":"string"},"max_results":{"type":"number"}},
            "required":["jql"]
        });
        assert_eq!(compact_schema(&s), "{jql:string,max_results?:number}");
    }

    #[test]
    fn nested_object_and_array() {
        let s = json!({
            "type":"object",
            "properties":{
                "filters":{"type":"array","items":{"type":"string"}},
                "page":{"type":"object","properties":{"size":{"type":"integer"}},"required":["size"]}
            },
            "required":["filters"]
        });
        assert_eq!(compact_schema(&s), "{filters:string[],page?:{size:number}}");
    }

    #[test]
    fn enum_and_const() {
        assert_eq!(compact_schema(&json!({"enum":["asc","desc"]})), "\"asc\"|\"desc\"");
        assert_eq!(compact_schema(&json!({"enum":[1,2,3]})), "1|2|3");
        assert_eq!(compact_schema(&json!({"const":"fixed"})), "\"fixed\"");
    }

    #[test]
    fn type_array_and_anyof_unions() {
        assert_eq!(compact_schema(&json!({"type":["string","null"]})), "string|null");
        assert_eq!(
            compact_schema(&json!({"anyOf":[{"type":"string"},{"type":"number"}]})),
            "string|number"
        );
    }

    #[test]
    fn array_of_union_is_parenthesized() {
        let s = json!({"type":"array","items":{"type":["string","number"]}});
        assert_eq!(compact_schema(&s), "(string|number)[]");
    }

    #[test]
    fn non_identifier_keys_are_quoted() {
        let s = json!({
            "type":"object",
            "properties":{"max-results":{"type":"number"}},
            "required":["max-results"]
        });
        assert_eq!(compact_schema(&s), "{\"max-results\":number}");
    }

    #[test]
    fn missing_type_falls_back_to_any() {
        assert_eq!(compact_schema(&json!({})), "any");
    }

    #[test]
    fn tool_signature_matches_spec_example() {
        let s = json!({
            "type":"object",
            "properties":{"jql":{"type":"string"},"max_results":{"type":"number"}},
            "required":["jql"]
        });
        assert_eq!(
            compact_tool_signature("search_jira_issues", &s),
            "type search_jira_issues = (p:{jql:string,max_results?:number})=>any;"
        );
    }

    #[test]
    fn safe_strips_metadata_keeps_structure() {
        let s = json!({
            "$schema":"http://json-schema.org/draft-07/schema#",
            "type":"object",
            "description":"a long tool description",
            "properties":{"a":{"type":"string","description":"d"}},
            "required":["a"],
            "additionalProperties":false
        });
        let out = compact_jsonschema(&s, false, 0);
        assert_eq!(
            out,
            json!({
                "type":"object",
                "properties":{"a":{"type":"string"}},
                "required":["a"],
                "additionalProperties":false
            })
        );
    }

    #[test]
    fn balanced_truncates_descriptions() {
        let s = json!({
            "type":"object",
            "properties":{"a":{"type":"string","description":"abcdefgh"}},
            "required":["a"]
        });
        let out = compact_jsonschema(&s, true, 4);
        assert_eq!(
            out,
            json!({
                "type":"object",
                "properties":{"a":{"type":"string","description":"abcd…"}},
                "required":["a"]
            })
        );
    }
}