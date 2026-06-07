//! JSON Schema validation + light rehydration for the invocation path.
//!
//! Why hand-rolled? The full `jsonschema` crate pulls a large regex tree that
//! would balloon our compile time and MSRV surface. Tool inputSchemas in
//! practice cover a narrow slice of draft-07: object-with-primitives,
//! occasional arrays, enums, unions. This module implements that slice
//! exactly, returns structured errors carrying a JSON-pointer-style path, and
//! gives us full control over what we enforce.
//!
//! Rehydration philosophy: because the compactor preserves real property
//! names across all tiers, the model's emitted arguments already match the
//! upstream's expected shape — true "condensed → verbose" mapping is mostly
//! identity. The one normalisation we do is for the aggressive `high` tier:
//! the model has freedom to emit `null` for optional fields against the
//! permissive `{ "type": "object" }` we exposed, so we strip those before
//! validation. Everything else is a straight validate-and-pass-through.

use std::fmt;

use serde_json::{Map, Value};

/// A precise validation failure: the JSON-pointer-like path into the args
/// where the problem occurred, plus a human-readable reason. The interceptor
/// surfaces both in the JSON-RPC error reply so the agent can self-correct.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub path: String,
    pub reason: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.reason)
        } else {
            write!(f, "{}: {}", self.path, self.reason)
        }
    }
}

impl std::error::Error for ValidationError {}

fn err(path: &str, reason: impl Into<String>) -> ValidationError {
    ValidationError {
        path: path.to_string(),
        reason: reason.into(),
    }
}

/// Validate `value` against `schema`. Errors carry a JSON-pointer-ish path.
pub fn validate(value: &Value, schema: &Value) -> Result<(), ValidationError> {
    validate_at(value, schema, "")
}

fn validate_at(value: &Value, schema: &Value, path: &str) -> Result<(), ValidationError> {
    // `const` — the value must equal the constant exactly.
    if let Some(c) = schema.get("const") {
        if value != c {
            return Err(err(path, format!("expected const {c}")));
        }
    }

    // `enum` — value must be one of the listed JSON values.
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.iter().any(|v| v == value) {
            return Err(err(path, format!("not in enum {values:?}")));
        }
    }

    // `anyOf` — at least one branch must match.
    if let Some(arr) = schema.get("anyOf").and_then(Value::as_array) {
        let mut last = None;
        let mut matched = false;
        for sub in arr {
            match validate_at(value, sub, path) {
                Ok(()) => {
                    matched = true;
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        if !matched {
            return Err(last.unwrap_or_else(|| err(path, "no anyOf branch matched")));
        }
    }

    // `oneOf` — exactly one branch must match.
    if let Some(arr) = schema.get("oneOf").and_then(Value::as_array) {
        let n = arr
            .iter()
            .filter(|s| validate_at(value, s, path).is_ok())
            .count();
        if n != 1 {
            return Err(err(
                path,
                format!("expected exactly 1 oneOf branch to match, got {n}"),
            ));
        }
    }

    // `allOf` — every branch must match.
    if let Some(arr) = schema.get("allOf").and_then(Value::as_array) {
        for sub in arr {
            validate_at(value, sub, path)?;
        }
    }

    // `type` (string or array of strings).
    if let Some(t) = schema.get("type") {
        match t {
            Value::String(name) => {
                if !type_matches(value, name) {
                    return Err(err(
                        path,
                        format!("type mismatch: expected {name}, got {}", value_type(value)),
                    ));
                }
            }
            Value::Array(names) => {
                let any_match = names
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|n| type_matches(value, n));
                if !any_match {
                    return Err(err(
                        path,
                        format!(
                            "type mismatch: expected one of {:?}, got {}",
                            names,
                            value_type(value)
                        ),
                    ));
                }
            }
            _ => {}
        }
    }

    // Type-specific keywords — driven off the *value*'s actual JSON type, so
    // a permissive schema (no `type`) still validates structural constraints.
    match value {
        Value::Object(map) => validate_object(map, schema, path)?,
        Value::Array(items) => validate_array(items, schema, path)?,
        Value::String(s) => validate_string(s, schema, path)?,
        Value::Number(_) => validate_number(value, schema, path)?,
        _ => {}
    }

    Ok(())
}

fn validate_object(
    map: &Map<String, Value>,
    schema: &Value,
    path: &str,
) -> Result<(), ValidationError> {
    // `required`
    if let Some(req) = schema.get("required").and_then(Value::as_array) {
        for r in req.iter().filter_map(Value::as_str) {
            if !map.contains_key(r) {
                return Err(err(path, format!("missing required property '{r}'")));
            }
        }
    }

    // `properties` + `additionalProperties`
    let props = schema.get("properties").and_then(Value::as_object);
    let additional = schema.get("additionalProperties");
    for (k, v) in map {
        let sub_path = format!("{path}/{k}");
        match props.and_then(|p| p.get(k)) {
            Some(sub_schema) => validate_at(v, sub_schema, &sub_path)?,
            None => match additional {
                Some(Value::Bool(false)) => {
                    return Err(err(path, format!("additional property '{k}' not allowed")));
                }
                Some(s @ Value::Object(_)) => validate_at(v, s, &sub_path)?,
                _ => {} // permitted by default
            },
        }
    }
    Ok(())
}

fn validate_array(items: &[Value], schema: &Value, path: &str) -> Result<(), ValidationError> {
    match schema.get("items") {
        // Homogeneous list — same schema for every element.
        Some(item_schema @ Value::Object(_)) => {
            for (i, v) in items.iter().enumerate() {
                validate_at(v, item_schema, &format!("{path}/{i}"))?;
            }
        }
        // Tuple form — each position has its own schema.
        Some(Value::Array(tuple)) => {
            for (i, v) in items.iter().enumerate() {
                if let Some(sub) = tuple.get(i) {
                    validate_at(v, sub, &format!("{path}/{i}"))?;
                }
            }
        }
        _ => {}
    }
    if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
        if (items.len() as u64) < min {
            return Err(err(path, format!("minItems: {} < {min}", items.len())));
        }
    }
    if let Some(max) = schema.get("maxItems").and_then(Value::as_u64) {
        if (items.len() as u64) > max {
            return Err(err(path, format!("maxItems: {} > {max}", items.len())));
        }
    }
    Ok(())
}

fn validate_string(s: &str, schema: &Value, path: &str) -> Result<(), ValidationError> {
    let n = s.chars().count() as u64;
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
        if n < min {
            return Err(err(path, format!("minLength: {n} < {min}")));
        }
    }
    if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
        if n > max {
            return Err(err(path, format!("maxLength: {n} > {max}")));
        }
    }
    Ok(())
}

fn validate_number(value: &Value, schema: &Value, path: &str) -> Result<(), ValidationError> {
    let f = value.as_f64().unwrap_or(f64::NAN);
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if f < min {
            return Err(err(path, format!("{f} < minimum {min}")));
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if f > max {
            return Err(err(path, format!("{f} > maximum {max}")));
        }
    }
    Ok(())
}

fn type_matches(value: &Value, name: &str) -> bool {
    match (name, value) {
        ("string", Value::String(_)) => true,
        ("number", Value::Number(_)) => true,
        // `integer` accepts both i/u64-backed numbers and whole-number floats —
        // the latter because some hosts serialize ints as floats.
        ("integer", Value::Number(n)) => {
            n.is_i64() || n.is_u64() || n.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false)
        }
        ("boolean", Value::Bool(_)) => true,
        ("null", Value::Null) => true,
        ("array", Value::Array(_)) => true,
        ("object", Value::Object(_)) => true,
        _ => false,
    }
}

fn value_type(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Null => "null",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Rehydration
// ---------------------------------------------------------------------------

/// Normalize args and validate them against the upstream's original schema.
///
/// `aggressive = true` (the `high` tier) strips explicit `null` values for
/// optional fields — under a permissive `{ "type": "object" }` projection,
/// the model often emits nulls that would fail the real schema.
pub fn rehydrate(
    args: &Value,
    schema: &Value,
    aggressive: bool,
) -> Result<Value, ValidationError> {
    let normalized = if aggressive {
        strip_optional_nulls(args, schema)
    } else {
        args.clone()
    };
    validate(&normalized, schema)?;
    Ok(normalized)
}

fn strip_optional_nulls(args: &Value, schema: &Value) -> Value {
    let (Value::Object(map), Some(props)) =
        (args, schema.get("properties").and_then(Value::as_object))
    else {
        return args.clone();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = Map::new();
    for (k, v) in map {
        if v.is_null() && !required.contains(&k.as_str()) {
            continue; // drop nulls for optional fields
        }
        if let Some(sub) = props.get(k) {
            out.insert(k.clone(), strip_optional_nulls(v, sub));
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema_jql() -> Value {
        json!({
            "type":"object",
            "properties":{"jql":{"type":"string"},"max_results":{"type":"integer","minimum":1,"maximum":100}},
            "required":["jql"]
        })
    }

    #[test]
    fn valid_object_passes() {
        assert!(validate(&json!({"jql":"project=AI"}), &schema_jql()).is_ok());
    }

    #[test]
    fn missing_required_fails_with_clear_reason() {
        let e = validate(&json!({}), &schema_jql()).unwrap_err();
        assert!(e.reason.contains("missing required"));
        assert!(e.reason.contains("jql"));
    }

    #[test]
    fn wrong_type_pinpoints_path() {
        let e = validate(&json!({"jql": 42}), &schema_jql()).unwrap_err();
        assert_eq!(e.path, "/jql");
        assert!(e.reason.contains("type mismatch"));
    }

    #[test]
    fn additional_properties_false_rejects_unknown() {
        let s = json!({
            "type":"object",
            "properties":{"a":{"type":"string"}},
            "additionalProperties":false
        });
        let e = validate(&json!({"a":"x","b":"y"}), &s).unwrap_err();
        assert!(e.reason.contains("additional property"));
    }

    #[test]
    fn enum_and_const() {
        let s = json!({"enum":["asc","desc"]});
        assert!(validate(&json!("asc"), &s).is_ok());
        assert!(validate(&json!("nope"), &s).is_err());
        assert!(validate(&json!("x"), &json!({"const":"x"})).is_ok());
        assert!(validate(&json!("y"), &json!({"const":"x"})).is_err());
    }

    #[test]
    fn integer_accepts_whole_floats_rejects_fractions() {
        let s = json!({"type":"integer"});
        assert!(validate(&json!(5), &s).is_ok());
        assert!(validate(&json!(5.0), &s).is_ok());
        assert!(validate(&json!(5.5), &s).is_err());
    }

    #[test]
    fn nested_path_in_error() {
        let s = json!({
            "type":"object",
            "properties":{"page":{"type":"object","properties":{"size":{"type":"integer"}},"required":["size"]}},
            "required":["page"]
        });
        let e = validate(&json!({"page":{"size":"big"}}), &s).unwrap_err();
        assert_eq!(e.path, "/page/size");
    }

    #[test]
    fn anyof_matches_any_branch() {
        let s = json!({"anyOf":[{"type":"string"},{"type":"number"}]});
        assert!(validate(&json!("x"), &s).is_ok());
        assert!(validate(&json!(5), &s).is_ok());
        assert!(validate(&json!(true), &s).is_err());
    }

    #[test]
    fn array_items_validated_with_index_path() {
        let s = json!({"type":"array","items":{"type":"string"}});
        assert!(validate(&json!(["a","b"]), &s).is_ok());
        let e = validate(&json!(["a", 2]), &s).unwrap_err();
        assert_eq!(e.path, "/1");
    }

    #[test]
    fn string_length_constraints() {
        let s = json!({"type":"string","minLength":2,"maxLength":4});
        assert!(validate(&json!("ab"), &s).is_ok());
        assert!(validate(&json!("a"), &s).is_err());
        assert!(validate(&json!("abcde"), &s).is_err());
    }

    #[test]
    fn number_range_constraints() {
        let s = json!({"type":"number","minimum":0,"maximum":100});
        assert!(validate(&json!(50), &s).is_ok());
        assert!(validate(&json!(-1), &s).is_err());
        assert!(validate(&json!(101), &s).is_err());
    }

    #[test]
    fn rehydrate_strips_optional_nulls_under_aggressive() {
        let s = schema_jql();
        let args = json!({"jql":"x","max_results":null});
        let out = rehydrate(&args, &s, true).unwrap();
        assert_eq!(out, json!({"jql":"x"}));
    }

    #[test]
    fn rehydrate_non_aggressive_passes_through_unchanged() {
        let s = json!({"type":"object","properties":{"a":{"type":"string"}},"required":["a"]});
        let args = json!({"a":"x"});
        assert_eq!(rehydrate(&args, &s, false).unwrap(), args);
    }

    #[test]
    fn rehydrate_caches_real_validation_failure() {
        let s = schema_jql();
        let e = rehydrate(&json!({"max_results": 5}), &s, true).unwrap_err();
        assert!(e.reason.contains("missing required"));
    }
}