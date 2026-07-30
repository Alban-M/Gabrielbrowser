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
    let response = &outcome.response;
    println!(
        "{} {}",
        style.bold(&outcome.sent.method),
        style.safe(&redactor.apply(&outcome.sent.url))
    );

    if verbose {
        for (name, value) in &outcome.sent.headers {
            println!(
                "  {} {}",
                style.dim(&style.safe(&format!("{name}:"))),
                style.safe(&redactor.apply(value))
            );
        }
        if let Some(body) = &outcome.sent.body {
            println!("{}", style.dim("  body:"));
            println!("{}", indent(&style.safe(&redactor.apply(body)), 4));
        }
        println!();
    }

    println!(
        "{} {} {} {} {}",
        style.status(response.status),
        style.dim(&response.status_text),
        style.dim("·"),
        format_duration(response.timings.total_ms),
        style.dim(&gabriel_core::format_bytes(response.size())),
    );

    if verbose {
        for (name, value) in response.headers.iter_pairs() {
            println!(
                "  {} {}",
                style.dim(&style.safe(&format!("{name}:"))),
                style.safe(value)
            );
        }
    }

    let body = render_body(response, body_limit);
    if !body.trim().is_empty() {
        println!();
        println!("{}", style.safe(&redactor.apply(&body)));
    }

    if !outcome.captured.is_empty() {
        println!();
        for (name, value) in &outcome.captured {
            println!(
                "{} {} = {}",
                style.cyan("captured"),
                style.safe(name),
                style.safe(&redactor.apply(&truncate(value, 120)))
            );
        }
    }

    if !outcome.assertions.is_empty() {
        println!();
        for assertion in &outcome.assertions {
            if assertion.passed {
                println!("{} {}", style.green("✓"), style.safe(&assertion.description));
            } else {
                println!(
                    "{} {} {}",
                    style.red("✗"),
                    style.safe(&assertion.description),
                    style.dim(&style.safe(&format!("(got {})", truncate(&assertion.actual, 80))))
                );
            }
        }
    }
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
        println!("{}", style.dim("body identical (not JSON — compared as bytes)"));
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

pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.2}s", ms as f64 / 1000.0)
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
        let style = Style { enabled: false, tty: false };
        assert_eq!(style.red("boom"), "boom");
        assert_eq!(style.status(500), "500");
    }

    #[test]
    fn escape_sequences_from_a_response_cannot_reach_the_terminal() {
        // "clear the line, go to column 0, print a reassuring lie"
        let hostile = "\x1b[2K\rgabriel: 0 vulnerabilities found";
        let printed = Style { enabled: true, tty: true }.safe(hostile);

        assert!(!printed.contains('\x1b'), "ESC survived: {printed:?}");
        assert!(!printed.contains('\r'), "CR survived: {printed:?}");
        assert!(printed.starts_with("^[[2K^M"), "unexpected rendering: {printed}");
        // The text itself is still readable, just inert.
        assert!(printed.contains("0 vulnerabilities found"));
    }

    #[test]
    fn other_control_bytes_are_neutralised_too() {
        let style = Style { enabled: true, tty: true };
        // Terminal title, bell, backspace-based overwriting, and a C1 escape.
        assert_eq!(style.safe("\x1b]0;title\x07"), "^[]0;title^G");
        assert_eq!(style.safe("secret\x08\x08\x08\x08\x08\x08public"), "secret^H^H^H^H^H^Hpublic");
        assert_eq!(style.safe("\u{9b}[31m"), "\\u{9b}[31m");
        assert_eq!(style.safe("\x7f"), "^?");
    }

    #[test]
    fn newlines_tabs_and_text_are_left_alone() {
        let style = Style { enabled: true, tty: true };
        assert_eq!(style.safe("line one\nline two\tindented"), "line one\nline two\tindented");
        assert_eq!(style.safe("café ☕ 日本語 🔒"), "café ☕ 日本語 🔒");
        assert_eq!(style.safe("{\"a\": 1}"), "{\"a\": 1}");
    }

    /// Piping must stay byte-exact: `gabriel run --quiet | jq` should receive
    /// what the server actually sent, and a pipe is not a terminal to attack.
    #[test]
    fn a_pipe_receives_the_bytes_unaltered() {
        let style = Style { enabled: false, tty: false };
        let hostile = "\x1b[2K\rtext";
        assert_eq!(style.safe(hostile), hostile);
    }

    #[test]
    fn durations_switch_units() {
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1500), "1.50s");
    }
}
