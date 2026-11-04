//! Rebuilding a request body from the keys the policy knows (#321).
//! Docker decodes JSON with Go's `encoding/json`, which matches keys
//! without regard to case and lets a later duplicate win: a body checked
//! as received could mean something else to the daemon. So the proxy never
//! forwards what it received. It forwards a new object made of the keys it
//! checked, spelled as Docker spells them. Any other key must hold its
//! zero value, which is also what the daemon assumes when the key is
//! absent, and is dropped.

use serde_json::{Map, Value};

/// Whether `v` is what Go decodes into a zero value: `null`, `false`, `0`,
/// `""`, or a list or object of such values.
pub fn is_zero(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.iter().all(is_zero),
        Value::Object(o) => o.values().all(is_zero),
    }
}

/// The object `v`, or why it is not one.
pub fn object<'a>(v: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    v.as_object()
        .ok_or_else(|| format!("{what} is not an object"))
}

/// The keys of `obj` that are in `kept`, exactly spelled. Any other key
/// must be zero (see the module's documentation) and is left out.
pub fn kept(
    obj: &Map<String, Value>,
    kept: &[&str],
    what: &str,
) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (key, value) in obj {
        if kept.contains(&key.as_str()) {
            out.insert(key.clone(), value.clone());
        } else if !is_zero(value) {
            return Err(format!("{what}.{key} is not allowed"));
        }
    }
    Ok(out)
}

pub fn string<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(format!("{key} is not a string")),
    }
}

pub fn bool(obj: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{key} is not a boolean")),
    }
}

pub fn integer(obj: &Map<String, Value>, key: &str) -> Result<i64, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v.as_i64().ok_or_else(|| format!("{key} is not an integer")),
    }
}

pub fn strings<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<Vec<&'a str>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|s| {
                s.as_str()
                    .ok_or_else(|| format!("{key} holds a non-string"))
            })
            .collect(),
        Some(_) => Err(format!("{key} is not a list")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn zero_is_what_go_assumes_of_an_absent_key() {
        for v in [
            json!(null),
            json!(false),
            json!(0),
            json!(""),
            json!([0, 0]),
            json!({ "a": "" }),
        ] {
            assert!(is_zero(&v), "{v}");
        }
        for v in [
            json!(true),
            json!(-1),
            json!("no"),
            json!([1]),
            json!({ "a": 1 }),
        ] {
            assert!(!is_zero(&v), "{v}");
        }
    }

    #[test]
    fn a_key_spelled_otherwise_must_be_zero() {
        let body = json!({ "Privileged": false, "privileged": true });
        let err = kept(body.as_object().unwrap(), &["Privileged"], "HostConfig").unwrap_err();
        assert!(err.contains("HostConfig.privileged"), "{err}");
        let body = json!({ "Memory": 1, "memory": 0, "Labels": {} });
        let out = kept(body.as_object().unwrap(), &["Memory"], "HostConfig").unwrap();
        assert_eq!(Value::Object(out), json!({ "Memory": 1 }));
    }
}
