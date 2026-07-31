//! The last output surface: what gets printed when Gabriel crashes.
//!
//! Every other surface is written deliberately, so redaction can be applied at
//! the point of writing. A panic is the opposite — it prints text nobody chose,
//! from a code path nobody expected to reach, through a hook installed by the
//! standard library. `.expect(&format!("failed for {url}"))` is an ordinary
//! thing to write, and a resolved URL can carry a token in its query string.
//!
//! So the hook is replaced with one that scrubs before printing. It uses both
//! defences the rest of the codebase uses:
//!
//! * the pattern scrubber, which catches credential *shapes* without being told
//!   anything, and
//! * a registry of the values actually resolved from the vault this run, which
//!   is the only way to catch a secret that looks like an ordinary word.
//!
//! The registry means resolved secrets sit in a process-wide list for the rest
//! of the run. That is a real trade-off and worth stating plainly: they are
//! already in this process's memory — held by the resolver, the request, and
//! the response — so the registry extends how long they are reachable, not
//! whether they are. For a command-line process that exits in seconds, printing
//! a credential to a terminal someone will screenshot is the larger risk.

use crate::feedback;
use std::panic::PanicHookInfo;
use std::sync::RwLock;

/// Values resolved from the vault this run. Read by the panic hook, which may
/// run on any thread at any moment, hence the lock.
static RESOLVED_SECRETS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Tell the panic hook about secrets that have been resolved.
///
/// Additive and idempotent: several requests in a `run --all` each contribute
/// what they used, and re-registering the same value changes nothing.
pub fn register_secrets(secrets: impl IntoIterator<Item = String>) {
    let Ok(mut held) = RESOLVED_SECRETS.write() else {
        // A poisoned lock means a previous panic happened mid-write. Losing a
        // registration is not worth a second panic inside the panic path.
        return;
    };
    for secret in secrets {
        if !secret.is_empty() && !held.contains(&secret) {
            held.push(secret);
        }
    }
}

/// Everything registered so far.
fn registered() -> Vec<String> {
    RESOLVED_SECRETS
        .read()
        .map(|held| held.clone())
        .unwrap_or_default()
}

/// Install the hook. Called once, from `main`.
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        eprint!("{}", render(info, &registered()));
    }));
}

/// Build what a panic prints.
///
/// Separated from the hook so it can be tested: a test cannot easily assert on
/// what the real hook wrote to the real stderr, and this is the part where a
/// secret would survive.
fn render(info: &PanicHookInfo<'_>, secrets: &[String]) -> String {
    // The payload is whatever was passed to `panic!` — a `&str` for a literal,
    // a `String` once it has been formatted, and neither for a panic raised by
    // something other than the macro.
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".to_string());

    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "an unknown location".to_string());

    let mut out = String::new();
    out.push_str("gabriel panicked. This is a bug.\n\n");
    out.push_str(&format!("  at {location}\n"));
    out.push_str(&format!("  {}\n", clean(&payload, secrets)));
    out.push('\n');

    // A backtrace names functions and files, not values, so it is safe to print
    // unscrubbed — and losing it would make a crash report much less useful.
    // Same opt-in as the standard hook.
    match std::env::var("RUST_BACKTRACE").as_deref() {
        Ok("1") | Ok("full") => {
            out.push_str(&format!("{}\n", std::backtrace::Backtrace::force_capture()));
        }
        _ => out.push_str("Set RUST_BACKTRACE=1 for a backtrace.\n"),
    }

    out.push_str("Please report it, with `gabriel feedback` attached if you can.\n");
    out
}

/// Both defences, in the order that matters: known values first, so a secret
/// that would also match a pattern is replaced by the more specific mask.
fn clean(text: &str, secrets: &[String]) -> String {
    let mut cleaned = text.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            cleaned = cleaned.replace(secret.as_str(), feedback::MASK);
        }
    }
    feedback::scrub(&cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gabriel_testkit::{assert_no_secret, canary};

    /// The panic hook is process-global, and the test binary runs tests in
    /// parallel — so two of these swapping hooks at once would each capture the
    /// other's panic. They take turns.
    static HOOK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `PanicHookInfo` cannot be constructed outside the standard library, so
    /// the hook is exercised by actually panicking and capturing what the hook
    /// produced.
    fn panic_output(message: String, secrets: &[String]) -> String {
        use std::sync::{Arc, Mutex};

        let _turn = HOOK.lock().unwrap_or_else(|e| e.into_inner());

        let captured: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&captured);
        let owned: Vec<String> = secrets.to_vec();

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            *sink.lock().unwrap() = render(info, &owned);
        }));
        let _ = std::panic::catch_unwind(|| panic!("{message}"));
        std::panic::set_hook(previous);

        captured.lock().unwrap().clone()
    }

    #[test]
    fn a_panic_quoting_a_resolved_url_carries_no_secret() {
        let secrets: Vec<String> = canary::ALL.iter().map(|s| s.to_string()).collect();
        let output = panic_output(
            format!(
                "failed for https://api.test/v1?key={} with {} and {}",
                canary::OPAQUE_TOKEN,
                canary::PLAIN_SECRET,
                canary::JWT
            ),
            &secrets,
        );
        assert_no_secret("panic message", &output);
    }

    /// The registry is what catches a secret that looks like a word. Without
    /// it, the pattern scrubber alone cannot know `orchidmoth` is a credential.
    #[test]
    fn a_word_shaped_secret_needs_the_registry() {
        let message = format!("failed for {}", canary::PLAIN_SECRET);

        let without = panic_output(message.clone(), &[]);
        assert!(
            without.contains(canary::PLAIN_SECRET),
            "the pattern scrubber should not be expected to catch this: {without}"
        );

        let with = panic_output(message, &[canary::PLAIN_SECRET.to_string()]);
        assert_no_secret("panic message", &with);
    }

    /// A shape-detectable secret is caught even if nothing registered it —
    /// a panic before the resolver has run, say.
    #[test]
    fn a_credential_shaped_value_is_caught_without_the_registry() {
        let output = panic_output(format!("token {}", canary::JWT), &[]);
        assert_no_secret("panic message", &output);
    }

    #[test]
    fn the_message_says_what_to_do() {
        let output = panic_output("something broke".into(), &[]);
        assert!(output.contains("This is a bug"), "{output}");
        assert!(output.contains("gabriel feedback"), "{output}");
        assert!(output.contains("RUST_BACKTRACE"), "{output}");
        // The location is what makes a report actionable.
        assert!(output.contains("panic.rs:"), "no location: {output}");
    }

    #[test]
    fn registering_is_additive_and_ignores_duplicates() {
        register_secrets(["one".to_string(), "two".to_string()]);
        register_secrets(["two".to_string(), "three".to_string()]);
        let held = registered();
        assert_eq!(held.iter().filter(|s| *s == "two").count(), 1, "{held:?}");
        for expected in ["one", "two", "three"] {
            assert!(held.iter().any(|s| s == expected), "{held:?}");
        }
    }

    #[test]
    fn an_empty_secret_is_never_registered() {
        // A blank would turn every character boundary into a match.
        register_secrets([String::new()]);
        assert!(!registered().iter().any(|s| s.is_empty()));
    }
}
