//! One invariant, asserted the same way everywhere: **no secret leaves the
//! process.**
//!
//! Gabriel handles credentials on behalf of someone else, so every surface
//! that emits text is a place a credential can escape — the terminal, JUnit and
//! HTML reports, generated curl commands, support bundles, error messages,
//! files written to disk. Each of those had its own ad-hoc assertion, written
//! by whoever added the surface, checking whatever they thought of at the time.
//! That is how a surface added later ends up with no check at all.
//!
//! This crate holds the canaries and the assertion. A test feeds a canary in
//! and calls [`assert_no_secret`] on whatever comes out; a new output surface
//! inherits the guarantee by using the same two things.
//!
//! ```no_run
//! # use gabriel_testkit::{canary, assert_no_secret};
//! let rendered = format!("token: {}", canary::OPAQUE_TOKEN);
//! assert_no_secret("terminal output", &rendered); // fails: it leaked
//! ```
//!
//! Dev-dependency only. Nothing here is compiled into the binary.

/// Values a test feeds in when it wants a secret to exist.
///
/// They come in two deliberate shapes, because a surface can be right for two
/// different reasons and only one of them generalises:
///
/// - **Shape-detectable** ([`OPAQUE_TOKEN`], [`JWT`]) — long, mixed-case,
///   digits and letters. A pattern scrubber catches these on its own.
/// - **Shape-invisible** ([`PLAIN_SECRET`], [`PASSWORD`]) — short, ordinary,
///   indistinguishable from prose. *Only* a surface that knows which values are
///   secret can redact these.
///
/// A surface that passes with the first group and fails with the second is
/// relying on pattern matching where it should be relying on knowing.
pub mod canary {
    /// Looks like an API key and is long enough for a scrubber to spot.
    pub const OPAQUE_TOKEN: &str = "sk-live-Ax7Qm2Rt9Zv4Kw8Nb3Hc6Jd1Pf5";

    /// A syntactically valid JWT. Payload decodes to `{"sub":"canary"}`.
    pub const JWT: &str =
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJjYW5hcnkifQ.9kZ1QeCanaryNotARealSig";

    /// Short, lowercase, no digits — nothing about it looks like a credential.
    /// Redacting this requires knowing it is one.
    pub const PLAIN_SECRET: &str = "orchidmoth";

    /// The other shape-invisible one, for surfaces that take a password.
    pub const PASSWORD: &str = "hunter2";

    /// A session cookie value.
    pub const COOKIE_VALUE: &str = "canaryjar";

    /// Every canary. What [`super::assert_no_secret`] looks for.
    pub const ALL: &[&str] = &[OPAQUE_TOKEN, JWT, PLAIN_SECRET, PASSWORD, COOKIE_VALUE];
}

/// Assert that no canary appears in `output`.
///
/// `surface` names what produced it, so a failure says which output surface
/// leaked rather than only which test noticed.
///
/// # Panics
///
/// If any value in [`canary::ALL`] appears in `output`.
#[track_caller]
pub fn assert_no_secret(surface: &str, output: &str) {
    assert_no_secret_of(surface, output, canary::ALL);
}

/// [`assert_no_secret`] with an explicit list, for a secret a test invented.
///
/// # Panics
///
/// If any value in `secrets` appears in `output`.
#[track_caller]
pub fn assert_no_secret_of(surface: &str, output: &str, secrets: &[&str]) {
    let mut leaked: Vec<(&str, String)> = Vec::new();

    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        if let Some(at) = output.find(secret) {
            leaked.push((secret, excerpt(output, at, secret.len())));
        }
    }

    if leaked.is_empty() {
        return;
    }

    let mut message = format!(
        "{} leaked {} secret{}:\n",
        surface,
        leaked.len(),
        if leaked.len() == 1 { "" } else { "s" }
    );
    for (secret, context) in &leaked {
        message.push_str(&format!("\n  {secret}\n  in: …{context}…\n"));
    }

    // A masked value sitting next to an unmasked one is the failure mode worth
    // naming out loud: redaction that ran, appeared to work, and missed.
    if output.contains("[redacted]") || output.contains("***") || output.contains("•••") {
        message.push_str(
            "\nThe output also contains a redaction marker, so something was masked and \
             this was missed — the dangerous kind, because it looks handled.\n",
        );
    }

    panic!("{message}");
}

/// A window around the leak, with control characters made visible so a match
/// inside an escape sequence is readable in the failure.
fn excerpt(output: &str, at: usize, len: usize) -> String {
    const PAD: usize = 30;
    let start = output[..at]
        .char_indices()
        .rev()
        .nth(PAD)
        .map_or(0, |(i, _)| i);
    let end_from = at + len;
    let end = output[end_from.min(output.len())..]
        .char_indices()
        .nth(PAD)
        .map_or(output.len(), |(i, _)| end_from + i);

    output[start..end]
        .chars()
        .map(|c| match c {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            '\x1b' => "\\e".to_string(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_output_passes() {
        assert_no_secret("test", "GET /users 200 OK in 41ms");
    }

    #[test]
    #[should_panic(expected = "leaked 1 secret")]
    fn a_leak_fails() {
        assert_no_secret(
            "test",
            &format!("Authorization: Bearer {}", canary::OPAQUE_TOKEN),
        );
    }

    #[test]
    #[should_panic(expected = "looks handled")]
    fn a_partial_redaction_is_called_out() {
        // The Bearer-token bug, in miniature: the label masked, the credential
        // still there.
        assert_no_secret(
            "test",
            &format!("Authorization: [redacted] {}", canary::PLAIN_SECRET),
        );
    }

    #[test]
    #[should_panic(expected = "reports")]
    fn the_failure_names_the_surface() {
        assert_no_secret("reports", canary::JWT);
    }

    #[test]
    fn every_canary_is_distinctive_enough_not_to_match_prose() {
        // A canary that collides with ordinary words would make every surface
        // look like it leaks.
        let prose = "The request failed after three attempts. \
             Check the collection, the environment, and the session store.";
        assert_no_secret("prose", prose);
    }

    #[test]
    fn the_excerpt_shows_where_it_leaked() {
        let output = format!(
            "line one\nAuthorization: Bearer {}\nline three",
            canary::JWT
        );
        let panic = std::panic::catch_unwind(|| assert_no_secret("t", &output)).unwrap_err();
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload should be a String");
        assert!(
            message.contains("\\n"),
            "newlines not made visible: {message}"
        );
        assert!(message.contains("Authorization"), "no context: {message}");
    }
}
