//! Turning a resolved request into a `curl` command.
//!
//! Two reasons this exists. Sharing: a curl line is the universal way to hand a
//! request to somebody, or paste it into a bug report. Migration: it makes
//! Gabriel a step on the way out as well as in, which is the point of an open
//! format.
//!
//! Secrets are masked unless explicitly asked for, because the overwhelmingly
//! common destination for a generated curl command is somewhere public.

use gabriel_core::vars::Redactor;
use gabriel_engine::SentRequest;

/// Headers curl sets itself, which would be noise (or wrong) if repeated.
const CURL_MANAGED: &[&str] = &["content-length", "host"];

/// `show` is the user explicitly asking for the real credentials. A generated
/// curl command is made to be pasted, so a header whose job is carrying a
/// credential is masked by name — the redactor only knows vault values, and a
/// session cookie is never one of them.
pub fn to_curl(request: &SentRequest, redactor: &Redactor, multiline: bool, show: bool) -> String {
    let joiner = if multiline { " \\\n  " } else { " " };
    let mut parts = vec!["curl".to_string()];

    if !request.method.eq_ignore_ascii_case("GET") {
        parts.push(format!("-X {}", request.method.to_uppercase()));
    }
    parts.push(quote(&redactor.apply(&request.url)));

    for (name, value) in &request.headers {
        if CURL_MANAGED.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        parts.push(format!(
            "-H {}",
            quote(&format!(
                "{name}: {}",
                gabriel_core::vars::header_for_display(name, value, redactor, show)
            ))
        ));
    }

    if let Some(body) = &request.body
        && !body.is_empty()
    {
        // `--data-raw` rather than `-d`: the latter strips newlines and would
        // silently change a JSON body's bytes.
        parts.push(format!("--data-raw {}", quote(&redactor.apply(body))));
    }

    parts.join(joiner)
}

/// Wrap in single quotes for a POSIX shell.
///
/// A single quote inside cannot be escaped within single quotes, so the string is
/// closed, an escaped quote is emitted, and it reopens — the standard
/// `'\''` dance. Getting this wrong produces a command that silently runs
/// something different from what was shown.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, url: &str) -> SentRequest {
        SentRequest {
            method: method.to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        }
    }

    fn plain() -> Redactor {
        Redactor::default()
    }

    #[test]
    fn a_get_omits_the_method_flag() {
        let command = to_curl(
            &request("GET", "https://api.test/users"),
            &plain(),
            false,
            false,
        );
        assert_eq!(command, "curl 'https://api.test/users'");
    }

    #[test]
    fn other_methods_are_explicit() {
        let command = to_curl(
            &request("delete", "https://api.test/users/1"),
            &plain(),
            false,
            false,
        );
        assert!(command.starts_with("curl -X DELETE "), "{command}");
    }

    #[test]
    fn headers_and_body_are_included() {
        let mut req = request("POST", "https://api.test/users");
        req.headers
            .push(("Content-Type".into(), "application/json".into()));
        req.headers
            .push(("Accept".into(), "application/json".into()));
        req.body = Some(r#"{"name":"ada"}"#.into());

        let command = to_curl(&req, &plain(), false, false);
        assert!(
            command.contains("-H 'Content-Type: application/json'"),
            "{command}"
        );
        assert!(
            command.contains("-H 'Accept: application/json'"),
            "{command}"
        );
        assert!(
            command.contains(r#"--data-raw '{"name":"ada"}'"#),
            "{command}"
        );
    }

    #[test]
    fn curl_managed_headers_are_dropped() {
        let mut req = request("POST", "https://api.test/x");
        req.headers.push(("Content-Length".into(), "42".into()));
        req.headers.push(("host".into(), "api.test".into()));
        req.headers.push(("X-Keep".into(), "yes".into()));

        let command = to_curl(&req, &plain(), false, false);
        assert!(
            !command.to_lowercase().contains("content-length"),
            "{command}"
        );
        assert!(!command.to_lowercase().contains("-h 'host"), "{command}");
        assert!(command.contains("X-Keep"), "{command}");
    }

    /// A generated command usually ends up in a chat message or an issue, so the
    /// default must not carry a live credential.
    #[test]
    fn secrets_are_masked_by_default() {
        let mut req = request("GET", "https://api.test/?key=sk-live-SECRET");
        req.headers
            .push(("Authorization".into(), "Bearer sk-live-SECRET".into()));
        req.body = Some(r#"{"token":"sk-live-SECRET"}"#.into());

        let redactor = Redactor::new(vec!["sk-live-SECRET".to_string()]);
        let command = to_curl(&req, &redactor, false, false);

        assert!(
            !command.contains("sk-live-SECRET"),
            "a secret survived:\n{command}"
        );
        assert_eq!(command.matches("redacted").count(), 3, "{command}");
    }

    /// Ask a real shell what it makes of the quoting.
    ///
    /// Inspecting the generated string for escape sequences proves nothing — the
    /// correct escaping contains the same bytes an injection attempt would. The
    /// only meaningful check is whether a POSIX shell hands the value back
    /// unchanged, as a single argument.
    ///
    /// Unix only: there is no `sh` on a Windows runner, and `quote` implements
    /// POSIX quoting, which is what a generated curl command is pasted into.
    #[cfg(unix)]
    fn shell_roundtrip(quoted: &str) -> String {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .output()
            .expect("run sh");
        assert!(output.status.success(), "the shell rejected: {quoted}");
        String::from_utf8(output.stdout).expect("utf-8")
    }

    #[cfg(unix)]
    #[test]
    fn a_value_trying_to_escape_the_quoting_survives_a_real_shell_intact() {
        for hostile in [
            r#"{"name":"'; rm -rf /; echo '"}"#,
            "'",
            "''",
            "$(whoami)",
            "`id`",
            "a'b\"c\\d",
            "$HOME ${PATH}",
            "line one\nline two",
            "; touch /tmp/gabriel-injection-check",
        ] {
            assert_eq!(
                shell_roundtrip(&quote(hostile)),
                hostile,
                "shell altered {hostile:?}"
            );
        }
        // And nothing was executed along the way.
        assert!(!std::path::Path::new("/tmp/gabriel-injection-check").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_generated_command_passes_the_body_through_unchanged() {
        let mut req = request("POST", "https://api.test/x");
        let body = r#"{"note":"it's a 'quoted' value","cmd":"$(echo hi)"}"#;
        req.body = Some(body.to_string());

        let command = to_curl(&req, &plain(), false, false);
        let arg = command
            .split_once("--data-raw ")
            .expect("data flag present")
            .1;
        assert_eq!(shell_roundtrip(arg), body);
    }

    #[cfg(unix)]
    #[test]
    fn a_url_with_a_quote_is_also_escaped() {
        let url = "https://api.test/?q='or'1'='1";
        let command = to_curl(&request("GET", url), &plain(), false, false);
        let arg = command.split_once("curl ").expect("curl prefix").1;
        assert_eq!(shell_roundtrip(arg), url);
    }

    #[test]
    fn multiline_form_uses_backslash_continuations() {
        let mut req = request("POST", "https://api.test/x");
        req.headers
            .push(("Accept".into(), "application/json".into()));
        let command = to_curl(&req, &plain(), true, false);

        assert!(
            command.contains(" \\\n  "),
            "expected continuations:\n{command}"
        );
        // Each continuation must be the last thing on its line for the shell to
        // join them.
        for line in command.lines().take(command.lines().count() - 1) {
            assert!(line.ends_with('\\'), "line does not continue: {line:?}");
        }
    }

    /// Runs on every platform, so quoting is not left completely untested where
    /// there is no shell to ask.
    #[test]
    fn quoting_wraps_and_escapes_every_embedded_quote() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        // Two quotes produce two escapes, and the result stays balanced.
        let quoted = quote("a'b'c");
        assert_eq!(quoted.matches(r"'\''").count(), 2);
        assert!(quoted.starts_with('\'') && quoted.ends_with('\''));
    }

    #[test]
    fn an_empty_body_adds_no_data_flag() {
        let mut req = request("POST", "https://api.test/x");
        req.body = Some(String::new());
        assert!(!to_curl(&req, &plain(), false, false).contains("--data-raw"));
    }
}

/// Generated curl commands, checked against the same invariant.
///
/// A curl command is made to be pasted — into a ticket, a chat, a runbook —
/// so it is the surface most likely to be shared deliberately.
#[cfg(test)]
mod no_secret_leaves_the_process {
    use super::*;
    use gabriel_testkit::{assert_no_secret, canary};

    fn a_request_full_of_secrets() -> SentRequest {
        SentRequest {
            method: "POST".into(),
            url: format!("https://api.test/login?key={}", canary::OPAQUE_TOKEN),
            headers: vec![
                ("Authorization".into(), format!("Bearer {}", canary::JWT)),
                ("Cookie".into(), format!("session={}", canary::COOKIE_VALUE)),
            ],
            body: Some(format!("password={}", canary::PASSWORD)),
        }
    }

    fn knows_every_secret() -> Redactor {
        Redactor::new(canary::ALL.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_generated_curl_command_carries_no_secret() {
        let command = to_curl(
            &a_request_full_of_secrets(),
            &knows_every_secret(),
            true,
            false,
        );
        assert_no_secret("curl codegen (multiline)", &command);
        let command = to_curl(
            &a_request_full_of_secrets(),
            &knows_every_secret(),
            false,
            false,
        );
        assert_no_secret("curl codegen (one line)", &command);
    }
}

/// The leak the canary suite missed, and why it missed it.
#[cfg(test)]
mod a_credential_the_redactor_never_saw {
    use super::*;
    use gabriel_testkit::assert_no_secret_of;

    fn request(method: &str, url: &str) -> SentRequest {
        SentRequest {
            method: method.to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        }
    }

    /// A session cookie never passes through the vault, so `used_secrets()`
    /// does not contain it and `Redactor` cannot mask it by value. The earlier
    /// canary test passed because it *seeded the redactor with the answer* —
    /// which no real session does. This one gives the redactor nothing.
    #[test]
    fn a_session_cookie_is_masked_with_an_empty_redactor() {
        let mut req = request("GET", "https://api.test/me");
        req.headers
            .push(("Cookie".into(), "session_id=abc123".into()));
        req.headers
            .push(("Authorization".into(), "Bearer tok_live_9f2".into()));

        let command = to_curl(&req, &Redactor::default(), false, false);
        assert_no_secret_of(
            "curl codegen with an unseeded redactor",
            &command,
            &["abc123", "tok_live_9f2"],
        );
    }

    #[test]
    fn show_secrets_is_still_an_escape_hatch() {
        let mut req = request("GET", "https://api.test/me");
        req.headers
            .push(("Cookie".into(), "session_id=abc123".into()));

        let command = to_curl(&req, &Redactor::default(), false, true);
        assert!(command.contains("abc123"), "--show-secrets stopped working");
    }

    /// Masking by header name must not swallow ordinary headers.
    #[test]
    fn ordinary_headers_survive() {
        let mut req = request("GET", "https://api.test/me");
        req.headers
            .push(("Accept".into(), "application/json".into()));

        let command = to_curl(&req, &Redactor::default(), false, false);
        assert!(command.contains("application/json"), "{command}");
    }
}
