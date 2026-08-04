use std::collections::HashMap;

use crate::Result;
use crate::error::ComposeError;

/// Interpolate `${VAR}`, `${VAR:-default}`, `${VAR:+alternate}`, and `$VAR` patterns
/// in all string values within a serde_yaml::Value tree.
///
/// Variable lookup order: (1) provided `env` map, (2) system environment variables.
/// `$$` produces a literal `$`.
pub fn interpolate(value: &mut serde_yaml::Value, env: &HashMap<String, String>) -> Result<()> {
    match value {
        serde_yaml::Value::Mapping(map) => {
            // Collect keys to avoid borrow issues
            let keys: Vec<serde_yaml::Value> = map.keys().cloned().collect();
            for key in keys {
                if let Some(v) = map.get_mut(&key) {
                    interpolate(v, env)?;
                }
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for item in seq.iter_mut() {
                interpolate(item, env)?;
            }
        }
        serde_yaml::Value::String(s) => {
            *s = interpolate_string(s, env)?;
        }
        _ => {}
    }
    Ok(())
}

/// Replace variable references in a single string.
fn interpolate_string(s: &str, env: &HashMap<String, String>) -> Result<String> {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' {
            match chars.peek() {
                Some('$') => {
                    // $$ -> literal $
                    chars.next();
                    result.push('$');
                }
                Some('{') => {
                    // ${...} form
                    chars.next(); // consume '{'
                    let mut expr = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => expr.push(c),
                            None => {
                                return Err(ComposeError::InterpolationError(format!(
                                    "unterminated ${{}}: missing closing brace in '${{{expr}'"
                                )));
                            }
                        }
                    }
                    result.push_str(&resolve_expression(&expr, env)?);
                }
                _ => {
                    // $VAR form — take alphanumeric + underscore chars
                    let mut var_name = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_alphanumeric() || c == '_' {
                            var_name.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if var_name.is_empty() {
                        result.push('$');
                    } else {
                        result.push_str(&resolve_var(&var_name, env));
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

/// Resolve a `${expr}` with modifiers: `:-` (default), `:+` (alternate), `:?` (error).
/// Bare `-`/`+` are unsupported and fall through to a plain lookup.
fn resolve_expression(expr: &str, env: &HashMap<String, String>) -> Result<String> {
    let (var, rest) = match expr.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        Some(idx) => (&expr[..idx], &expr[idx..]),
        None => (expr, ""),
    };

    if var.is_empty() {
        return Ok(resolve_var(expr, env));
    }

    let val = resolve_var(var, env);

    if let Some(default) = rest.strip_prefix(":-") {
        Ok(if val.is_empty() { default.to_string() } else { val })
    } else if let Some(alternate) = rest.strip_prefix(":+") {
        Ok(if val.is_empty() { String::new() } else { alternate.to_string() })
    } else if let Some(msg) = rest.strip_prefix(":?") {
        if val.is_empty() {
            let message = if msg.is_empty() {
                format!("required variable '{var}' is not set or is empty")
            } else {
                msg.to_string()
            };
            Err(ComposeError::InterpolationError(message))
        } else {
            Ok(val)
        }
    } else if rest.is_empty() {
        Ok(val)
    } else {
        // Unknown operator suffix (e.g. bare `-`/`+`); resolve as a plain lookup.
        Ok(resolve_var(expr, env))
    }
}

/// Look up a plain variable name in env, falling back to system env.
fn resolve_var(name: &str, env: &HashMap<String, String>) -> String {
    if let Some(val) = env.get(name) {
        val.clone()
    } else if let Ok(val) = std::env::var(name) {
        val
    } else {
        String::new()
    }
}

/// Build an environment map from extra_env KEY=VALUE strings and system env vars.
/// Extra env takes priority over system env.
pub fn build_env_map(extra_env: &[String]) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    for item in extra_env {
        if let Some((k, v)) = item.split_once('=') {
            env.insert(k.to_string(), v.to_string());
        }
    }
    env
}

/// Seed literal `environment:` values for cross-field interpolation (e.g.
/// `command: ${DB_URL}`). Templated values (`$`) are skipped so self-referential
/// vars resolve in place instead of to their own literal.
pub fn extract_compose_env(compose_value: &serde_yaml::Value) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let services = match compose_value.get("services").and_then(|s| s.as_mapping()) {
        Some(m) => m,
        None => return env,
    };
    for (_, svc) in services {
        if let Some(env_val) = svc.get("environment").and_then(|e| e.as_mapping()) {
            for (k, v) in env_val {
                if let (Some(key), Some(val)) = (k.as_str(), v.as_str()) {
                    if !val.contains('$') {
                        env.insert(key.to_string(), val.to_string());
                    }
                }
            }
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_var() {
        let mut env = HashMap::new();
        env.insert("PORT".to_string(), "8080".to_string());

        let mut val = serde_yaml::Value::String("${PORT}".to_string());
        interpolate(&mut val, &env).unwrap();
        assert_eq!(val.as_str().unwrap(), "8080");
    }

    #[test]
    fn test_bare_var() {
        let mut env = HashMap::new();
        env.insert("HOST".to_string(), "localhost".to_string());

        let mut val = serde_yaml::Value::String("$HOST".to_string());
        interpolate(&mut val, &env).unwrap();
        assert_eq!(val.as_str().unwrap(), "localhost");
    }

    #[test]
    fn test_default_value() {
        let mut env = HashMap::new();
        // MISSING_VAR is not set

        let mut val = serde_yaml::Value::String("${MISSING_VAR:-3306}".to_string());
        interpolate(&mut val, &env).unwrap();
        assert_eq!(val.as_str().unwrap(), "3306");
    }

    #[test]
    fn test_alternate_value() {
        let mut env = HashMap::new();
        env.insert("DEBUG".to_string(), "1".to_string());

        let mut val = serde_yaml::Value::String("${DEBUG:+verbose}".to_string());
        interpolate(&mut val, &env).unwrap();
        assert_eq!(val.as_str().unwrap(), "verbose");
    }

    #[test]
    fn test_alternate_unset() {
        let env = HashMap::new();
        // DEBUG is not set

        let mut val = serde_yaml::Value::String("${DEBUG:+verbose}".to_string());
        interpolate(&mut val, &env).unwrap();
        assert_eq!(val.as_str().unwrap(), "");
    }

    #[test]
    fn test_escaped_dollar() {
        let mut val = serde_yaml::Value::String("$$HOME".to_string());
        interpolate(&mut val, &HashMap::new()).unwrap();
        assert_eq!(val.as_str().unwrap(), "$HOME");
    }

    #[test]
    fn test_unset_empty() {
        let mut val = serde_yaml::Value::String("${NONEXISTENT}".to_string());
        interpolate(&mut val, &HashMap::new()).unwrap();
        assert_eq!(val.as_str().unwrap(), "");
    }

    #[test]
    fn test_nested_in_mapping() {
        let mut env = HashMap::new();
        env.insert("DB_HOST".to_string(), "postgres".to_string());

        let yaml = "environment:\n  DATABASE_URL: postgres://${DB_HOST}:5432/mydb";
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        interpolate(&mut value, &env).unwrap();

        let url = value.get("environment").unwrap().get("DATABASE_URL").unwrap().as_str().unwrap();
        assert_eq!(url, "postgres://postgres:5432/mydb");
    }

    #[test]
    fn test_default_with_empty_var() {
        let mut env = HashMap::new();
        env.insert("PORT".to_string(), "".to_string());

        let mut val = serde_yaml::Value::String("${PORT:-3000}".to_string());
        interpolate(&mut val, &env).unwrap();
        // :- checks if empty, should use default
        assert_eq!(val.as_str().unwrap(), "3000");
    }

    #[test]
    fn test_build_env_map() {
        let env = build_env_map(&["FOO=bar".to_string(), "BAZ=qux".to_string()]);
        assert_eq!(env.get("FOO").unwrap(), "bar");
        assert_eq!(env.get("BAZ").unwrap(), "qux");
    }

    // ── Self-referential env vars (the original bug) ─────────────────────────

    #[test]
    fn test_self_referential_default_when_unset() {
        // A service defines `PUBLIC_URL: ${PUBLIC_URL:-https://x}` and PUBLIC_URL
        // is not provided anywhere. extract_compose_env must NOT seed the literal,
        // so the `:-` default fires instead of resolving to the raw `${...}` text.
        let yaml = "services:\n  web:\n    environment:\n      PUBLIC_URL: ${PUBLIC_URL:-https://cms.example.com}\n";
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        let compose_env = extract_compose_env(&value);
        // Templated value must not be seeded.
        assert!(!compose_env.contains_key("PUBLIC_URL"));

        let env = build_env_map(&[]);
        interpolate(&mut value, &env).unwrap();
        let url = value["services"]["web"]["environment"]["PUBLIC_URL"].as_str().unwrap();
        assert_eq!(url, "https://cms.example.com");
    }

    #[test]
    fn test_self_referential_uses_extra_env_when_provided() {
        // When the var IS provided via extra_env, the self-referential reference
        // resolves to that value rather than the default.
        let yaml = "services:\n  web:\n    environment:\n      PUBLIC_URL: ${PUBLIC_URL:-https://default}\n";
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        let mut env = build_env_map(&[]);
        env.insert("PUBLIC_URL".to_string(), "https://override".to_string());
        interpolate(&mut value, &env).unwrap();
        let url = value["services"]["web"]["environment"]["PUBLIC_URL"].as_str().unwrap();
        assert_eq!(url, "https://override");
    }

    // ── `:?` error-if-unset ───────────────────────────────────────────────────

    #[test]
    fn test_required_var_set() {
        let mut env = HashMap::new();
        env.insert("SECRET".to_string(), "abc123".to_string());

        let mut val = serde_yaml::Value::String("${SECRET:?SECRET is required}".to_string());
        interpolate(&mut val, &env).unwrap();
        assert_eq!(val.as_str().unwrap(), "abc123");
    }

    #[test]
    fn test_required_var_unset_errors() {
        let env = HashMap::new();
        let mut val = serde_yaml::Value::String("${SECRET:?SECRET is required}".to_string());
        let err = interpolate(&mut val, &env).unwrap_err();
        match err {
            ComposeError::InterpolationError(msg) => assert_eq!(msg, "SECRET is required"),
            other => panic!("expected InterpolationError, got {other:?}"),
        }
    }

    #[test]
    fn test_required_var_empty_errors() {
        let mut env = HashMap::new();
        env.insert("SECRET".to_string(), "".to_string());

        let mut val = serde_yaml::Value::String("${SECRET:?}".to_string());
        let err = interpolate(&mut val, &env).unwrap_err();
        match err {
            ComposeError::InterpolationError(msg) => assert!(msg.contains("SECRET"), "msg was: {msg}"),
            other => panic!("expected InterpolationError, got {other:?}"),
        }
    }

    // ── Cross-field literal resolution still works ────────────────────────────

    #[test]
    fn test_cross_field_literal_env_reference() {
        // A literal env value is available for interpolation of OTHER fields.
        let yaml = "services:\n  web:\n    environment:\n      DB_URL: postgres://x\n    command: app --db ${DB_URL}\n";
        let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        // Mirror parse_with_interpolation: literal env values are seeded for cross-field use.
        let mut env = build_env_map(&[]);
        env.extend(extract_compose_env(&value));
        interpolate(&mut value, &env).unwrap();
        let cmd = value["services"]["web"]["command"].as_str().unwrap();
        assert_eq!(cmd, "app --db postgres://x");
    }
}
