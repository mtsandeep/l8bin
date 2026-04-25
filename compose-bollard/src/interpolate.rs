use std::collections::HashMap;

use crate::error::ComposeError;
use crate::Result;

/// Interpolate `${VAR}`, `${VAR:-default}`, `${VAR:+alternate}`, and `$VAR` patterns
/// in all string values within a serde_yaml::Value tree.
///
/// Variable lookup order: (1) provided `env` map, (2) system environment variables.
/// `$$` produces a literal `$`.
pub fn interpolate(
    value: &mut serde_yaml::Value,
    env: &HashMap<String, String>,
) -> Result<()> {
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
                                return Err(ComposeError::InterpolationError(
                                    format!("unterminated ${{}}: missing closing brace in '${{{expr}'"),
                                ));
                            }
                        }
                    }
                    result.push_str(&resolve_expression(&expr, env));
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

/// Resolve a `${expr}` expression which may contain modifiers like `:-` or `:+`.
fn resolve_expression(expr: &str, env: &HashMap<String, String>) -> String {
    if let Some(idx) = expr.find(":-") {
        let var = &expr[..idx];
        let default = &expr[idx + 2..];
        let val = resolve_var(var, env);
        if val.is_empty() {
            default.to_string()
        } else {
            val
        }
    } else if let Some(idx) = expr.find(":+") {
        let var = &expr[..idx];
        let alternate = &expr[idx + 2..];
        let val = resolve_var(var, env);
        if val.is_empty() {
            String::new()
        } else {
            alternate.to_string()
        }
    } else {
        resolve_var(expr, env)
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

/// Pre-extract environment variable definitions from the compose file's
/// `environment` sections so they are available for interpolation of other values.
/// Values that themselves contain `${}` are NOT interpolated (they are the definitions).
pub fn extract_compose_env(
    compose_value: &serde_yaml::Value,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let services = match compose_value.get("services").and_then(|s| s.as_mapping()) {
        Some(m) => m,
        None => return env,
    };
    for (_, svc) in services {
        if let Some(env_val) = svc.get("environment").and_then(|e| e.as_mapping()) {
            for (k, v) in env_val {
                if let (Some(key), Some(val)) = (k.as_str(), v.as_str()) {
                    env.insert(key.to_string(), val.to_string());
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
        let env = HashMap::new();
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
}
