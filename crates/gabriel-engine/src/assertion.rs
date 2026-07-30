//! Assertion evaluation.
//!
//! Assertions are declared in the request file and evaluated against the
//! response, so the same file that documents a call also checks it — and the
//! same file runs in CI.

use gabriel_core::jsonpath;
use gabriel_core::model::{AssertOp, AssertTarget, Assertion};
use gabriel_core::response::ExecutedResponse;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct AssertionOutcome {
    /// Human-readable restatement of what was checked, e.g. `status == 200`.
    pub description: String,
    pub passed: bool,
    /// What was actually found, for the failure message.
    pub actual: String,
}

pub fn evaluate(assertion: &Assertion, response: &ExecutedResponse) -> AssertionOutcome {
    let expected = assertion.value.as_ref().map(toml_to_json);

    let actual: Option<Value> = match assertion.target {
        AssertTarget::Status => Some(Value::from(response.status)),
        AssertTarget::DurationMs => Some(Value::from(response.timings.total_ms)),
        AssertTarget::Header => assertion
            .path
            .as_deref()
            .and_then(|name| response.headers.get_first(name))
            .map(|v| Value::String(v.to_string())),
        AssertTarget::Body => match &assertion.path {
            Some(path) => match response.json() {
                Some(json) => jsonpath::select(&json, path).ok().flatten().cloned(),
                None => None,
            },
            None => Some(Value::String(response.text().into_owned())),
        },
    };

    let passed = match assertion.op {
        AssertOp::Exists => actual.is_some(),
        AssertOp::Missing => actual.is_none(),
        _ => match (&actual, &expected) {
            (Some(actual), Some(expected)) => compare(assertion.op, actual, expected),
            _ => false,
        },
    };

    AssertionOutcome {
        description: describe(assertion, expected.as_ref()),
        passed,
        actual: actual
            .as_ref()
            .map(render)
            .unwrap_or_else(|| "<missing>".to_string()),
    }
}

fn compare(op: AssertOp, actual: &Value, expected: &Value) -> bool {
    match op {
        AssertOp::Eq => values_equal(actual, expected),
        AssertOp::Ne => !values_equal(actual, expected),
        AssertOp::Lt => match (actual.as_f64(), expected.as_f64()) {
            (Some(a), Some(b)) => a < b,
            _ => false,
        },
        AssertOp::Gt => match (actual.as_f64(), expected.as_f64()) {
            (Some(a), Some(b)) => a > b,
            _ => false,
        },
        AssertOp::Contains => render(actual).contains(&render(expected)),
        AssertOp::Exists | AssertOp::Missing => unreachable!("handled by the caller"),
    }
}

/// `200` from TOML and `200` from JSON are the same number even when one is an
/// integer and the other a float; a string `"200"` is not.
fn values_equal(actual: &Value, expected: &Value) -> bool {
    if actual == expected {
        return true;
    }
    match (actual.as_f64(), expected.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn render(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn describe(assertion: &Assertion, expected: Option<&Value>) -> String {
    let target = match (assertion.target, &assertion.path) {
        (AssertTarget::Status, _) => "status".to_string(),
        (AssertTarget::DurationMs, _) => "duration_ms".to_string(),
        (AssertTarget::Header, Some(name)) => format!("header {name}"),
        (AssertTarget::Header, None) => "header".to_string(),
        (AssertTarget::Body, Some(path)) => format!("body {path}"),
        (AssertTarget::Body, None) => "body".to_string(),
    };
    let op = match assertion.op {
        AssertOp::Eq => "==",
        AssertOp::Ne => "!=",
        AssertOp::Lt => "<",
        AssertOp::Gt => ">",
        AssertOp::Contains => "contains",
        AssertOp::Exists => "exists",
        AssertOp::Missing => "is missing",
    };
    match expected {
        Some(value) => format!("{target} {op} {}", render(value)),
        None => format!("{target} {op}"),
    }
}

pub fn toml_to_json(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => Value::from(*f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(dt) => Value::String(dt.to_string()),
        toml::Value::Array(items) => Value::Array(items.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => Value::Object(
            table
                .iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gabriel_core::model::FieldMap;
    use gabriel_core::response::Timings;

    fn response(status: u16, body: &str) -> ExecutedResponse {
        let mut headers = FieldMap::default();
        headers.set("Content-Type", "application/json");
        headers.set("X-Region", "eu-west-1");
        ExecutedResponse {
            status,
            status_text: "OK".into(),
            http_version: "HTTP/2".into(),
            headers,
            body: body.as_bytes().to_vec(),
            timings: Timings {
                ttfb_ms: 10,
                total_ms: 25,
            },
            final_url: "https://api.test/users".into(),
        }
    }

    fn assertion(
        target: AssertTarget,
        path: Option<&str>,
        op: AssertOp,
        value: Option<toml::Value>,
    ) -> Assertion {
        Assertion {
            target,
            path: path.map(str::to_string),
            op,
            value,
        }
    }

    #[test]
    fn status_equality() {
        let outcome = evaluate(
            &assertion(
                AssertTarget::Status,
                None,
                AssertOp::Eq,
                Some(toml::Value::Integer(200)),
            ),
            &response(200, "{}"),
        );
        assert!(outcome.passed);
        assert_eq!(outcome.description, "status == 200");
    }

    #[test]
    fn a_failure_reports_what_was_actually_seen() {
        let outcome = evaluate(
            &assertion(
                AssertTarget::Status,
                None,
                AssertOp::Eq,
                Some(toml::Value::Integer(200)),
            ),
            &response(500, "{}"),
        );
        assert!(!outcome.passed);
        assert_eq!(outcome.actual, "500");
    }

    #[test]
    fn body_paths_are_addressed_with_jsonpath() {
        let response = response(200, r#"{"data":{"items":[{"id":7}]}}"#);
        let outcome = evaluate(
            &assertion(
                AssertTarget::Body,
                Some("data.items[0].id"),
                AssertOp::Eq,
                Some(toml::Value::Integer(7)),
            ),
            &response,
        );
        assert!(outcome.passed, "{outcome:?}");
    }

    #[test]
    fn headers_are_matched_case_insensitively() {
        let outcome = evaluate(
            &assertion(
                AssertTarget::Header,
                Some("x-region"),
                AssertOp::Eq,
                Some(toml::Value::String("eu-west-1".into())),
            ),
            &response(200, "{}"),
        );
        assert!(outcome.passed);
    }

    #[test]
    fn exists_and_missing_need_no_expected_value() {
        let response = response(200, r#"{"id":1}"#);
        assert!(
            evaluate(
                &assertion(AssertTarget::Body, Some("id"), AssertOp::Exists, None),
                &response
            )
            .passed
        );
        assert!(
            evaluate(
                &assertion(AssertTarget::Body, Some("nope"), AssertOp::Missing, None),
                &response
            )
            .passed
        );
        assert!(
            !evaluate(
                &assertion(AssertTarget::Body, Some("nope"), AssertOp::Exists, None),
                &response
            )
            .passed
        );
    }

    #[test]
    fn numeric_comparisons() {
        let response = response(200, "{}");
        assert!(
            evaluate(
                &assertion(
                    AssertTarget::DurationMs,
                    None,
                    AssertOp::Lt,
                    Some(toml::Value::Integer(1000))
                ),
                &response
            )
            .passed
        );
        assert!(
            !evaluate(
                &assertion(
                    AssertTarget::DurationMs,
                    None,
                    AssertOp::Gt,
                    Some(toml::Value::Integer(1000))
                ),
                &response
            )
            .passed
        );
    }

    #[test]
    fn contains_works_on_the_whole_body() {
        let response = response(200, r#"{"message":"created"}"#);
        assert!(
            evaluate(
                &assertion(
                    AssertTarget::Body,
                    None,
                    AssertOp::Contains,
                    Some(toml::Value::String("created".into()))
                ),
                &response
            )
            .passed
        );
    }

    #[test]
    fn a_string_does_not_equal_a_number() {
        let response = response(200, "{}");
        let outcome = evaluate(
            &assertion(
                AssertTarget::Status,
                None,
                AssertOp::Eq,
                Some(toml::Value::String("200".into())),
            ),
            &response,
        );
        assert!(!outcome.passed, "type confusion would hide real failures");
    }

    #[test]
    fn a_missing_body_path_fails_rather_than_erroring() {
        let response = response(200, "not json at all");
        let outcome = evaluate(
            &assertion(
                AssertTarget::Body,
                Some("id"),
                AssertOp::Eq,
                Some(toml::Value::Integer(1)),
            ),
            &response,
        );
        assert!(!outcome.passed);
        assert_eq!(outcome.actual, "<missing>");
    }
}
