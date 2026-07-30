//! Terminal formatting.
//!
//! Colour is applied only when stdout is a terminal, so piping to a file or a
//! CI log yields clean text. Every response body printed here passes through
//! the redactor first: a secret that came out of the vault must not be put back
//! on screen.

use gabriel_core::ExecutedResponse;
use gabriel_core::diff::{Change, ChangeKind, ResponseDiff};
use gabriel_core::vars::Redactor;
use gabriel_engine::RunOutcome;
use std::io::IsTerminal;

pub struct Style {
    enabled: bool,
    /// Whether stdout is a terminal. Tracked separately from colour because
    /// `NO_COLOR` turns colour off without making the terminal any less
    /// vulnerable to escape sequences.
    tty: bool,
}

impl Style {
    pub fn detect() -> Self {
        let tty = std::io::stdout().is_terminal();
        // NO_COLOR is a de-facto standard; honour it.
        let enabled = tty && std::env::var_os("NO_COLOR").is_none();
        Style { enabled, tty }
    }

    /// Make server-controlled text safe to print.
    ///
    /// Response bodies, header values and URLs are written by whoever is on the
    /// other end of the connection. Printed raw, an escape sequence in any of
    /// them can erase Gabriel's own output and replace it with something
    /// convincing — for a tool whose job is inspecting hostile traffic, that is
    /// not acceptable. Control bytes become caret notation (`^[` for ESC), which
    /// `cat -v` users will recognise and no terminal will act on.
    ///
    /// Only applied when stdout is a terminal: a pipe should receive the bytes
    /// that actually arrived.
    pub fn safe(&self, text: &str) -> String {
        if !self.tty {
            return text.to_string();
        }
        escape_controls(text)
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    pub fn green(&self, text: &str) -> String {
        self.paint("32", text)
    }

    pub fn red(&self, text: &str) -> String {
        self.paint("31", text)
    }

    pub fn yellow(&self, text: &str) -> String {
        self.paint("33", text)
    }

    pub fn cyan(&self, text: &str) -> String {
        self.paint("36", text)
    }

    pub fn status(&self, status: u16) -> String {
        let text = format!("{status}");
        match status {
            200..=299 => self.green(&text),
            300..=399 => self.cyan(&text),
            400..=499 => self.yellow(&text),
            _ => self.red(&text),
        }
    }
}

/// Replace control characters with caret notation, leaving newlines and tabs
/// alone so multi-line output still reads normally.
fn escape_controls(text: &str) -> String {
    if !text.chars().any(needs_escaping) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' | '\t' => out.push(ch),
            // C0 controls and DEL.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push('^');
                out.push(char::from_u32((c as u32 ^ 0x40) & 0x7f).unwrap_or('?'));
            }
            // C1 controls, which some terminals honour as escape equivalents.
            c if (0x80..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn needs_escaping(ch: char) -> bool {
    let code = ch as u32;
    (code < 0x20 && ch != '\n' && ch != '\t') || code == 0x7f || (0x80..0xa0).contains(&code)
}

/// Print a completed run: what was sent, what came back, what it bound, and
/// whether the assertions held.
pub fn print_run(
    outcome: &RunOutcome,
    style: &Style,
    redactor: &Redactor,
    verbose: bool,
    body_limit: usize,
) {
    let mut stdout = std::io::stdout().lock();
    // Errors here mean stdout is gone (closed pipe); nothing useful to do.
    let _ = write_run(&mut stdout, outcome, style, redactor, verbose, body_limit);
}

/// The body of [`print_run`], writing somewhere testable.
///
/// This function is where redaction and escape-defusing are composed over every
/// field, which makes it security-relevant: a field printed without both is a
/// leak or an injection. Writing to a generic sink lets a test assert on the
/// exact bytes a terminal would receive.
pub fn write_run<W: std::io::Write>(
    w: &mut W,
    outcome: &RunOutcome,
    style: &Style,
    redactor: &Redactor,
    verbose: bool,
    body_limit: usize,
) -> std::io::Result<()> {
    let response = &outcome.response;
    writeln!(
        w,
        "{} {}",
        style.bold(&outcome.sent.method),
        style.safe(&redactor.apply(&outcome.sent.url))
    )?;

    if verbose {
        for (name, value) in &outcome.sent.headers {
            writeln!(
                w,
                "  {} {}",
                style.dim(&style.safe(&format!("{name}:"))),
                style.safe(&redactor.apply(value))
            )?;
        }
        if let Some(body) = &outcome.sent.body {
            writeln!(w, "{}", style.dim("  body:"))?;
            writeln!(w, "{}", indent(&style.safe(&redactor.apply(body)), 4))?;
        }
        writeln!(w)?;
    }

    writeln!(
        w,
        "{} {} {} {} {}",
        style.status(response.status),
        style.dim(&response.status_text),
        style.dim("·"),
        format_duration(response.timings.total_ms),
        style.dim(&gabriel_core::format_bytes(response.size())),
    )?;

    if verbose {
        for (name, value) in response.headers.iter_pairs() {
            // Redacted as well as escaped: a response can echo the very token
            // that was sent (debug endpoints and signed-URL services both do
            // it), and `--show-secrets` being off has to mean off in both
            // directions.
            writeln!(
                w,
                "  {} {}",
                style.dim(&style.safe(&format!("{name}:"))),
                style.safe(&redactor.apply(value))
            )?;
        }
    }

    let body = render_body(response, body_limit);
    if !body.trim().is_empty() {
        writeln!(w)?;
        writeln!(w, "{}", style.safe(&redactor.apply(&body)))?;
    }

    if !outcome.captured.is_empty() {
        writeln!(w)?;
        for (name, value) in &outcome.captured {
            writeln!(
                w,
                "{} {} = {}",
                style.cyan("captured"),
                style.safe(name),
                style.safe(&redactor.apply(&truncate(value, 120)))
            )?;
        }
    }

    if !outcome.assertions.is_empty() {
        writeln!(w)?;
        for assertion in &outcome.assertions {
            if assertion.passed {
                writeln!(
                    w,
                    "{} {}",
                    style.green("✓"),
                    style.safe(&assertion.description)
                )?;
            } else {
                writeln!(
                    w,
                    "{} {} {}",
                    style.red("✗"),
                    style.safe(&assertion.description),
                    style.dim(&style.safe(&format!("(got {})", truncate(&assertion.actual, 80))))
                )?;
            }
        }
    }
    Ok(())
}

/// Pretty-print JSON; leave anything else alone; never dump raw binary.
pub fn render_body(response: &ExecutedResponse, limit: usize) -> String {
    if response.body.is_empty() {
        return String::new();
    }
    if let Some(json) = response.json() {
        let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();
        return truncate(&pretty, limit);
    }
    match std::str::from_utf8(&response.body) {
        Ok(text) => truncate(text, limit),
        Err(_) => format!(
            "<{} of binary data>",
            gabriel_core::format_bytes(response.body.len())
        ),
    }
}

pub fn print_diff(diff: &ResponseDiff, style: &Style) {
    if let Some((before, after)) = diff.status {
        println!(
            "{} {} → {}",
            style.bold("status"),
            style.status(before),
            style.status(after)
        );
    }
    if !diff.headers.is_empty() {
        println!("{}", style.bold("headers"));
        for change in &diff.headers {
            print_change(change, style);
        }
    }
    if !diff.body.is_empty() {
        println!("{}", style.bold("body"));
        for change in &diff.body {
            print_change(change, style);
        }
    } else if diff.body_is_opaque {
        println!(
            "{}",
            style.dim("body identical (not JSON — compared as bytes)")
        );
    }

    let (before, after) = diff.duration_ms;
    println!(
        "{}",
        style.dim(&format!("timing {}ms → {}ms", before, after))
    );

    if diff.is_empty() {
        println!("{}", style.green("no differences"));
    }
}

fn print_change(change: &Change, style: &Style) {
    let path = style.safe(&change.path);
    match &change.kind {
        ChangeKind::Added { after } => println!(
            "  {} {} {}",
            style.green("+"),
            path,
            style.dim(&style.safe(&truncate(after, 100)))
        ),
        ChangeKind::Removed { before } => println!(
            "  {} {} {}",
            style.red("-"),
            path,
            style.dim(&style.safe(&truncate(before, 100)))
        ),
        ChangeKind::Changed { before, after } => println!(
            "  {} {} {} → {}",
            style.yellow("~"),
            path,
            style.dim(&style.safe(&truncate(before, 60))),
            style.safe(&truncate(after, 60))
        ),
    }
}

/// Durations at whatever scale they arrive: request timings are milliseconds,
/// token lifetimes are hours. "899.36s" is technically correct and useless.
pub fn format_duration(ms: u64) -> String {
    const SECOND: u64 = 1000;
    const MINUTE: u64 = 60 * SECOND;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;

    match ms {
        ms if ms < SECOND => format!("{ms}ms"),
        ms if ms < MINUTE => format!("{:.2}s", ms as f64 / SECOND as f64),
        ms if ms < HOUR => {
            let minutes = ms / MINUTE;
            let seconds = (ms % MINUTE) / SECOND;
            if seconds == 0 {
                format!("{minutes}m")
            } else {
                format!("{minutes}m {seconds}s")
            }
        }
        ms if ms < DAY => {
            let hours = ms / HOUR;
            let minutes = (ms % HOUR) / MINUTE;
            if minutes == 0 {
                format!("{hours}h")
            } else {
                format!("{hours}h {minutes}m")
            }
        }
        ms => {
            let days = ms / DAY;
            let hours = (ms % DAY) / HOUR;
            if hours == 0 {
                format!("{days}d")
            } else {
                format!("{days}d {hours}h")
            }
        }
    }
}

pub fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}… ({} more characters)", text.chars().count() - limit)
}

fn indent(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.lines()
        .map(|line| format!("{pad}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gabriel_core::model::FieldMap;
    use gabriel_core::response::Timings;

    fn response(headers: &[(&str, &str)], body: &[u8]) -> ExecutedResponse {
        ExecutedResponse {
            status: 200,
            status_text: "OK".into(),
            http_version: "HTTP/2".into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<FieldMap>(),
            body: body.to_vec(),
            timings: Timings::default(),
            final_url: "https://api.test".into(),
        }
    }

    #[test]
    fn json_bodies_are_pretty_printed() {
        let rendered = render_body(
            &response(&[("Content-Type", "application/json")], br#"{"a":1}"#),
            10_000,
        );
        assert!(rendered.contains("\n  \"a\": 1"), "{rendered}");
    }

    #[test]
    fn binary_bodies_are_described_not_dumped() {
        let rendered = render_body(&response(&[], &[0u8, 159, 146, 150]), 10_000);
        assert!(rendered.contains("binary"), "{rendered}");
    }

    #[test]
    fn long_bodies_are_truncated_with_a_count() {
        let long = "x".repeat(500);
        let rendered = render_body(&response(&[], long.as_bytes()), 100);
        assert!(rendered.contains("400 more characters"), "{rendered}");
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate("héllo", 10), "héllo");
        assert!(truncate("héllo wörld", 5).starts_with("héllo"));
    }

    #[test]
    fn styles_are_plain_when_disabled() {
        let style = Style {
            enabled: false,
            tty: false,
        };
        assert_eq!(style.red("boom"), "boom");
        assert_eq!(style.status(500), "500");
    }

    #[test]
    fn escape_sequences_from_a_response_cannot_reach_the_terminal() {
        // "clear the line, go to column 0, print a reassuring lie"
        let hostile = "\x1b[2K\rgabriel: 0 vulnerabilities found";
        let printed = Style {
            enabled: true,
            tty: true,
        }
        .safe(hostile);

        assert!(!printed.contains('\x1b'), "ESC survived: {printed:?}");
        assert!(!printed.contains('\r'), "CR survived: {printed:?}");
        assert!(
            printed.starts_with("^[[2K^M"),
            "unexpected rendering: {printed}"
        );
        // The text itself is still readable, just inert.
        assert!(printed.contains("0 vulnerabilities found"));
    }

    #[test]
    fn other_control_bytes_are_neutralised_too() {
        let style = Style {
            enabled: true,
            tty: true,
        };
        // Terminal title, bell, backspace-based overwriting, and a C1 escape.
        assert_eq!(style.safe("\x1b]0;title\x07"), "^[]0;title^G");
        assert_eq!(
            style.safe("secret\x08\x08\x08\x08\x08\x08public"),
            "secret^H^H^H^H^H^Hpublic"
        );
        assert_eq!(style.safe("\u{9b}[31m"), "\\u{9b}[31m");
        assert_eq!(style.safe("\x7f"), "^?");
    }

    #[test]
    fn newlines_tabs_and_text_are_left_alone() {
        let style = Style {
            enabled: true,
            tty: true,
        };
        assert_eq!(
            style.safe("line one\nline two\tindented"),
            "line one\nline two\tindented"
        );
        assert_eq!(style.safe("café ☕ 日本語 🔒"), "café ☕ 日本語 🔒");
        assert_eq!(style.safe("{\"a\": 1}"), "{\"a\": 1}");
    }

    /// Piping must stay byte-exact: `gabriel run --quiet | jq` should receive
    /// what the server actually sent, and a pipe is not a terminal to attack.
    #[test]
    fn a_pipe_receives_the_bytes_unaltered() {
        let style = Style {
            enabled: false,
            tty: false,
        };
        let hostile = "\x1b[2K\rtext";
        assert_eq!(style.safe(hostile), hostile);
    }

    fn hostile_outcome() -> gabriel_engine::RunOutcome {
        use gabriel_engine::{RunOutcome, SentRequest};
        let body = concat!(
            r#"{"echoed_token":"sk-live-CANARY-7788","#,
            "\"lie\":\"\x1b[2K\rgabriel: 0 problems\"}"
        );
        let mut headers = FieldMap::default();
        headers.set("Content-Type", "application/json");
        headers.set("X-Reflected", "sk-live-CANARY-7788");

        RunOutcome {
            sent: SentRequest {
                method: "GET".into(),
                url: "https://api.test/?token=sk-live-CANARY-7788".into(),
                headers: vec![("Authorization".into(), "Bearer sk-live-CANARY-7788".into())],
                body: Some("{\"token\":\"sk-live-CANARY-7788\"}".into()),
            },
            response: ExecutedResponse {
                status: 200,
                status_text: "OK".into(),
                http_version: "HTTP/2".into(),
                headers,
                body: body.as_bytes().to_vec(),
                timings: Timings::default(),
                final_url: "https://api.test/".into(),
            },
            assertions: Vec::new(),
            captured: vec![("token".into(), "sk-live-CANARY-7788".into())],
            redirects: Vec::new(),
        }
    }

    /// The security-relevant composition: every field printed must be both
    /// redacted *and* escape-defused. Testing the two helpers separately does
    /// not prove they are actually applied together at every print site — which
    /// is exactly where a leak would hide.
    #[test]
    fn nothing_printed_leaks_a_secret_or_an_escape_sequence() {
        let style = Style {
            enabled: true,
            tty: true,
        };
        let redactor = Redactor::new(vec!["sk-live-CANARY-7788".to_string()]);

        let mut out = Vec::new();
        write_run(
            &mut out,
            &hostile_outcome(),
            &style,
            &redactor,
            true,
            10_000,
        )
        .unwrap();
        let printed = String::from_utf8(out).expect("utf-8 output");

        assert!(
            !printed.contains("sk-live-CANARY-7788"),
            "a secret reached the output:\n{printed}"
        );
        // Gabriel's own colour codes are ESC too, so look for the payload's.
        assert!(
            !printed.contains("\x1b[2K") && !printed.contains('\r'),
            "a hostile escape sequence survived:\n{printed:?}"
        );
        // And the output is still useful.
        assert!(printed.contains("redacted"));
        assert!(
            printed.contains("gabriel: 0 problems"),
            "text should survive, inert"
        );
    }

    #[test]
    fn secrets_are_masked_in_the_url_headers_body_and_captures() {
        let style = Style {
            enabled: false,
            tty: false,
        };
        let redactor = Redactor::new(vec!["sk-live-CANARY-7788".to_string()]);

        let mut out = Vec::new();
        write_run(
            &mut out,
            &hostile_outcome(),
            &style,
            &redactor,
            true,
            10_000,
        )
        .unwrap();
        let printed = String::from_utf8(out).unwrap();

        // Six distinct places the same secret appears: the URL, the request's
        // Authorization header, the request body, a response header echoing it
        // back, the response body, and the captured variable. All six masked.
        assert_eq!(
            printed.matches("••••redacted••••").count(),
            6,
            "a secret escaped one of the six print sites:\n{printed}"
        );
    }

    /// A response header echoing a secret is a real pattern (debug endpoints do
    /// it), and it must not be printed just because it came back rather than
    /// went out.
    #[test]
    fn a_secret_reflected_in_a_response_header_is_masked_too() {
        let style = Style {
            enabled: false,
            tty: false,
        };
        let redactor = Redactor::new(vec!["sk-live-CANARY-7788".to_string()]);

        let mut out = Vec::new();
        write_run(
            &mut out,
            &hostile_outcome(),
            &style,
            &redactor,
            true,
            10_000,
        )
        .unwrap();
        let printed = String::from_utf8(out).unwrap();

        let reflected = printed
            .lines()
            .find(|l| l.to_lowercase().contains("x-reflected"))
            .expect("the reflected header should be printed");
        assert!(!reflected.contains("sk-live-CANARY"), "leaked: {reflected}");
    }

    #[test]
    fn durations_switch_units() {
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1500), "1.50s");
        // Two decimals means the last millisecond before a minute rounds up;
        // harmless, and cheaper than special-casing the boundary.
        assert_eq!(format_duration(59_999), "60.00s");
        assert_eq!(format_duration(59_500), "59.50s");
        // A token lifetime should read as minutes, not "899.36s".
        assert_eq!(format_duration(899_360), "14m 59s");
        assert_eq!(format_duration(900_000), "15m");
        assert_eq!(format_duration(3_600_000), "1h");
        assert_eq!(format_duration(5_400_000), "1h 30m");
        assert_eq!(format_duration(86_400_000), "1d");
        assert_eq!(format_duration(90_000_000), "1d 1h");
    }
}

/// The terminal, checked against the same invariant as every other surface.
///
/// Terminal output is the surface people screenshot and paste into chat, so a
/// credential reaching it travels about as far as one in a report.
#[cfg(test)]
mod no_secret_leaves_the_process {
    use super::*;
    use gabriel_core::model::FieldMap;
    use gabriel_core::response::Timings;
    use gabriel_testkit::{assert_no_secret, canary};

    /// A response that has put a secret into every field the printer renders:
    /// the URL, a request header, the request body, a response header, the
    /// response body, and a captured variable.
    fn an_outcome_full_of_secrets() -> gabriel_engine::RunOutcome {
        use gabriel_engine::{RunOutcome, SentRequest};

        let mut headers = FieldMap::default();
        headers.set("Content-Type", "application/json");
        headers.set("X-Reflected", canary::OPAQUE_TOKEN);
        headers.set("Set-Cookie", format!("session={}", canary::COOKIE_VALUE));

        RunOutcome {
            sent: SentRequest {
                method: "POST".into(),
                url: format!("https://api.test/?token={}", canary::OPAQUE_TOKEN),
                headers: vec![
                    ("Authorization".into(), format!("Bearer {}", canary::JWT)),
                    ("X-Password".into(), canary::PASSWORD.into()),
                ],
                body: Some(format!("{{\"pass\":\"{}\"}}", canary::PLAIN_SECRET)),
            },
            response: ExecutedResponse {
                status: 200,
                status_text: "OK".into(),
                http_version: "HTTP/2".into(),
                headers,
                body: format!("{{\"echo\":\"{}\"}}", canary::PLAIN_SECRET).into_bytes(),
                timings: Timings::default(),
                final_url: format!("https://api.test/done?t={}", canary::OPAQUE_TOKEN),
            },
            assertions: Vec::new(),
            captured: vec![("token".into(), canary::OPAQUE_TOKEN.into())],
            redirects: Vec::new(),
        }
    }

    fn printed(verbose: bool, tty: bool) -> String {
        let style = Style { enabled: tty, tty };
        let redactor = Redactor::new(canary::ALL.iter().map(|s| s.to_string()).collect());
        let mut out = Vec::new();
        write_run(
            &mut out,
            &an_outcome_full_of_secrets(),
            &style,
            &redactor,
            verbose,
            10_000,
        )
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_terminal_carries_no_secret() {
        assert_no_secret("terminal output (verbose, tty)", &printed(true, true));
    }

    #[test]
    fn the_quiet_path_carries_no_secret() {
        assert_no_secret("terminal output (quiet)", &printed(false, false));
    }

    /// A pipe gets byte-exact output so `--quiet | jq` works, which means the
    /// escape-defusing step is skipped. Redaction must not be skipped with it.
    #[test]
    fn a_pipe_carries_no_secret_either() {
        assert_no_secret("piped output", &printed(true, false));
    }
}
