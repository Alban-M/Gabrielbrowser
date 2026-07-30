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
}

impl Style {
    pub fn detect() -> Self {
        // NO_COLOR is a de-facto standard; honour it.
        let enabled = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Style { enabled }
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
        redactor.apply(&outcome.sent.url)
    );

    if verbose {
        for (name, value) in &outcome.sent.headers {
            println!("  {} {}", style.dim(&format!("{name}:")), redactor.apply(value));
        }
        if let Some(body) = &outcome.sent.body {
            println!("{}", style.dim("  body:"));
            println!("{}", indent(&redactor.apply(body), 4));
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
            println!("  {} {}", style.dim(&format!("{name}:")), value);
        }
    }

    let body = render_body(response, body_limit);
    if !body.trim().is_empty() {
        println!();
        println!("{}", redactor.apply(&body));
    }

    if !outcome.captured.is_empty() {
        println!();
        for (name, value) in &outcome.captured {
            println!(
                "{} {} = {}",
                style.cyan("captured"),
                name,
                redactor.apply(&truncate(value, 120))
            );
        }
    }

    if !outcome.assertions.is_empty() {
        println!();
        for assertion in &outcome.assertions {
            if assertion.passed {
                println!("{} {}", style.green("✓"), assertion.description);
            } else {
                println!(
                    "{} {} {}",
                    style.red("✗"),
                    assertion.description,
                    style.dim(&format!("(got {})", truncate(&assertion.actual, 80)))
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
    match &change.kind {
        ChangeKind::Added { after } => println!(
            "  {} {} {}",
            style.green("+"),
            change.path,
            style.dim(&truncate(after, 100))
        ),
        ChangeKind::Removed { before } => println!(
            "  {} {} {}",
            style.red("-"),
            change.path,
            style.dim(&truncate(before, 100))
        ),
        ChangeKind::Changed { before, after } => println!(
            "  {} {} {} → {}",
            style.yellow("~"),
            change.path,
            style.dim(&truncate(before, 60)),
            truncate(after, 60)
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
        let style = Style { enabled: false };
        assert_eq!(style.red("boom"), "boom");
        assert_eq!(style.status(500), "500");
    }

    #[test]
    fn durations_switch_units() {
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1500), "1.50s");
    }
}
