//! `{{template}}` resolution.
//!
//! Every string in a request — URL, header values, body text, auth tokens —
//! passes through here before execution. Four namespaces are supported:
//!
//! | Syntax              | Source                                            |
//! |---------------------|---------------------------------------------------|
//! | `{{name}}`          | environment variables and run-time captures       |
//! | `{{secret:name}}`   | the encrypted vault, never the collection files   |
//! | `{{env:NAME}}`      | the process environment (opt-in)                  |
//! | `{{$uuid}}` etc.    | generated values                                  |
//!
//! The resolver records every secret value it substitutes so that the same
//! values can be masked again on the way out — a token that came from the vault
//! should never end up in a terminal, a log, or a screenshot.

use crate::error::{Error, Result};
use crate::model::{FieldMap, FieldValue};
use std::collections::{BTreeMap, BTreeSet};

/// Supplies secrets by name. Implemented by the vault; `()` supplies nothing.
pub trait SecretProvider {
    fn secret(&self, key: &str) -> Option<String>;
}

impl SecretProvider for () {
    fn secret(&self, _key: &str) -> Option<String> {
        None
    }
}

impl SecretProvider for BTreeMap<String, String> {
    fn secret(&self, key: &str) -> Option<String> {
        self.get(key).cloned()
    }
}

const MAX_DEPTH: usize = 8;

pub struct Resolver<'a> {
    vars: BTreeMap<String, String>,
    secrets: &'a dyn SecretProvider,
    allow_process_env: bool,
    /// Plaintext secret values that have been substituted, for later redaction.
    used_secrets: BTreeSet<String>,
}

impl Default for Resolver<'static> {
    fn default() -> Self {
        Resolver::new()
    }
}

impl Resolver<'static> {
    pub fn new() -> Self {
        Resolver {
            vars: BTreeMap::new(),
            secrets: &(),
            allow_process_env: false,
            used_secrets: BTreeSet::new(),
        }
    }
}

impl<'a> Resolver<'a> {
    pub fn with_secrets(mut self, secrets: &'a dyn SecretProvider) -> Resolver<'a> {
        self.secrets = secrets;
        self
    }

    pub fn with_process_env(mut self, allow: bool) -> Self {
        self.allow_process_env = allow;
        self
    }

    pub fn with_vars(mut self, vars: BTreeMap<String, String>) -> Self {
        self.vars.extend(vars);
        self
    }

    /// Bind one variable, overriding any earlier value. Used for `--var` flags
    /// and for values captured out of a previous response.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    /// Secret plaintexts seen so far, longest first so that redaction replaces
    /// the most specific match when one secret contains another.
    pub fn used_secrets(&self) -> Vec<String> {
        let mut list: Vec<String> = self.used_secrets.iter().cloned().collect();
        list.sort_by_key(|s| std::cmp::Reverse(s.len()));
        list
    }

    pub fn resolve(&mut self, template: &str) -> Result<String> {
        let mut stack = Vec::new();
        self.expand(template, 0, &mut stack)
    }

    pub fn resolve_map(&mut self, map: &FieldMap) -> Result<FieldMap> {
        let mut out = BTreeMap::new();
        for (key, value) in &map.0 {
            let key = self.resolve(key)?;
            let value = match value {
                FieldValue::One(s) => FieldValue::One(self.resolve(s)?),
                FieldValue::Many(list) => FieldValue::Many(
                    list.iter()
                        .map(|s| self.resolve(s))
                        .collect::<Result<Vec<_>>>()?,
                ),
            };
            out.insert(key, value);
        }
        Ok(FieldMap(out))
    }

    fn expand(&mut self, template: &str, depth: usize, stack: &mut Vec<String>) -> Result<String> {
        if depth > MAX_DEPTH {
            return Err(Error::VariableRecursion(
                stack
                    .last()
                    .cloned()
                    .unwrap_or_else(|| template.to_string()),
            ));
        }
        // Fast path: most strings contain no template at all.
        if !template.contains("{{") {
            return Ok(template.to_string());
        }

        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                return Err(Error::UnterminatedTemplate(template.to_string()));
            };
            let name = after[..end].trim();
            let value = self.lookup(name, depth, stack)?;
            out.push_str(&value);
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        Ok(out)
    }

    fn lookup(&mut self, name: &str, depth: usize, stack: &mut Vec<String>) -> Result<String> {
        if let Some(key) = name.strip_prefix("secret:") {
            let key = key.trim();
            let value = self
                .secrets
                .secret(key)
                .ok_or_else(|| Error::UnknownSecret(key.to_string()))?;
            if !value.is_empty() {
                self.used_secrets.insert(value.clone());
            }
            // Secrets are terminal: their contents are never re-expanded, so a
            // secret whose value happens to contain `{{` can't be used to reach
            // other variables.
            return Ok(value);
        }

        if let Some(key) = name.strip_prefix("env:") {
            let key = key.trim();
            if !self.allow_process_env {
                return Err(Error::Invalid(format!(
                    "`{{{{env:{key}}}}}` needs --allow-env; process environment access is opt-in"
                )));
            }
            return std::env::var(key).map_err(|_| Error::UnknownVariable(format!("env:{key}")));
        }

        if let Some(generated) = generated_value(name) {
            return Ok(generated);
        }

        let raw = self
            .vars
            .get(name)
            .cloned()
            .ok_or_else(|| Error::UnknownVariable(name.to_string()))?;

        if stack.iter().any(|n| n == name) {
            return Err(Error::VariableRecursion(name.to_string()));
        }
        stack.push(name.to_string());
        let expanded = self.expand(&raw, depth + 1, stack);
        stack.pop();
        expanded
    }
}

fn generated_value(name: &str) -> Option<String> {
    match name {
        "$uuid" => Some(uuid::Uuid::new_v4().to_string()),
        "$timestamp" => Some(crate::now_ms().to_string()),
        "$isoTimestamp" => Some(crate::format_iso8601(crate::now_ms())),
        "$randomInt" => Some(rand::random_range(0..1_000_000u32).to_string()),
        _ => None,
    }
}

/// Masks known secret plaintexts in anything on its way to a screen or a file.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    pub fn new(secrets: Vec<String>) -> Self {
        // Ignore trivially short values: masking every "1" in a response would
        // be worse than useless.
        let secrets = secrets.into_iter().filter(|s| s.len() >= 4).collect();
        Redactor { secrets }
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), "••••redacted••••");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver_with(pairs: &[(&str, &str)]) -> Resolver<'static> {
        let vars = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Resolver::new().with_vars(vars)
    }

    #[test]
    fn substitutes_plain_variables() {
        let mut r = resolver_with(&[("base_url", "https://api.test")]);
        assert_eq!(
            r.resolve("{{base_url}}/users?x=1").unwrap(),
            "https://api.test/users?x=1"
        );
    }

    #[test]
    fn tolerates_whitespace_inside_braces() {
        let mut r = resolver_with(&[("host", "api.test")]);
        assert_eq!(r.resolve("{{  host }}").unwrap(), "api.test");
    }

    #[test]
    fn expands_variables_that_reference_variables() {
        let mut r = resolver_with(&[("base_url", "https://{{host}}"), ("host", "api.test")]);
        assert_eq!(r.resolve("{{base_url}}/v1").unwrap(), "https://api.test/v1");
    }

    #[test]
    fn rejects_self_referencing_variables() {
        let mut r = resolver_with(&[("a", "{{b}}"), ("b", "{{a}}")]);
        let err = r.resolve("{{a}}").unwrap_err();
        assert!(matches!(err, Error::VariableRecursion(_)), "got {err:?}");
    }

    #[test]
    fn unknown_variable_names_itself_in_the_error() {
        let mut r = Resolver::new();
        let err = r.resolve("{{missing}}").unwrap_err().to_string();
        assert!(err.contains("missing"), "unhelpful error: {err}");
    }

    #[test]
    fn unterminated_template_is_an_error() {
        let mut r = Resolver::new();
        assert!(matches!(
            r.resolve("{{oops").unwrap_err(),
            Error::UnterminatedTemplate(_)
        ));
    }

    #[test]
    fn secrets_come_from_the_provider_and_are_recorded() {
        let secrets: BTreeMap<String, String> =
            [("api_token".to_string(), "sk-live-abc123".to_string())].into();
        let mut r = Resolver::new().with_secrets(&secrets);
        assert_eq!(
            r.resolve("Bearer {{secret:api_token}}").unwrap(),
            "Bearer sk-live-abc123"
        );
        assert_eq!(r.used_secrets(), vec!["sk-live-abc123"]);
    }

    #[test]
    fn secret_values_are_not_re_expanded() {
        let secrets: BTreeMap<String, String> =
            [("weird".to_string(), "{{base_url}}".to_string())].into();
        let mut r = Resolver::new()
            .with_secrets(&secrets)
            .with_vars([("base_url".to_string(), "https://api.test".to_string())].into());
        assert_eq!(r.resolve("{{secret:weird}}").unwrap(), "{{base_url}}");
    }

    #[test]
    fn process_env_requires_opt_in() {
        let mut r = Resolver::new();
        assert!(r.resolve("{{env:PATH}}").is_err());
        let mut r = Resolver::new().with_process_env(true);
        assert!(!r.resolve("{{env:PATH}}").unwrap().is_empty());
    }

    #[test]
    fn generates_uuid_and_timestamp() {
        let mut r = Resolver::new();
        let id = r.resolve("{{$uuid}}").unwrap();
        assert_eq!(id.len(), 36);
        let ts: u64 = r.resolve("{{$timestamp}}").unwrap().parse().unwrap();
        assert!(ts > 1_700_000_000_000);
    }

    #[test]
    fn maps_have_both_keys_and_values_resolved() {
        let mut r = resolver_with(&[("header_name", "X-Tenant"), ("tenant", "acme")]);
        let mut map = FieldMap::default();
        map.set("{{header_name}}", "{{tenant}}");
        map.insert("Accept", "application/json");

        let resolved = r.resolve_map(&map).unwrap();
        assert_eq!(resolved.get_first("X-Tenant"), Some("acme"));
        assert_eq!(resolved.get_first("Accept"), Some("application/json"));
    }

    #[test]
    fn multi_valued_entries_are_all_resolved() {
        let mut r = resolver_with(&[("a", "one"), ("b", "two")]);
        let mut map = FieldMap::default();
        map.insert("tag", "{{a}}");
        map.insert("tag", "{{b}}");

        let resolved = r.resolve_map(&map).unwrap();
        let values: Vec<&str> = resolved.iter_pairs().map(|(_, v)| v).collect();
        assert_eq!(values, vec!["one", "two"]);
    }

    #[test]
    fn an_unknown_variable_in_a_map_is_an_error_not_a_blank() {
        let mut r = Resolver::new();
        let mut map = FieldMap::default();
        map.set("X-Trace", "{{missing}}");
        assert!(r.resolve_map(&map).is_err());
    }

    #[test]
    fn redactor_masks_recorded_secrets() {
        let redactor = Redactor::new(vec!["sk-live-abc123".into()]);
        let masked = redactor.apply("token=sk-live-abc123 ok");
        assert!(!masked.contains("sk-live-abc123"), "{masked}");
    }

    #[test]
    fn redactor_ignores_very_short_values() {
        let redactor = Redactor::new(vec!["ab".into()]);
        assert_eq!(redactor.apply("about"), "about");
    }
}
