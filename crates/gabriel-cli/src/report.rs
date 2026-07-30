//! Run reports: JUnit XML for CI, HTML for people.
//!
//! Two audiences, two formats. A CI system wants a machine-readable verdict it
//! can turn into a red or green build and a list of what broke; a human opening
//! the artifact afterwards wants to see the request, the response and the
//! assertion that failed without reading XML.
//!
//! Both formats embed data the server controlled — URLs, response bodies,
//! assertion values. Two consequences drive the implementation:
//!
//! * **Everything is escaped.** An unescaped `<` in a response body produces
//!   invalid XML that no CI parser will accept, and unescaped HTML in a report
//!   a build server renders is stored XSS. Escaping is not cosmetic here.
//! * **Secrets are redacted.** A report is an artifact: uploaded, archived, and
//!   often world-readable inside an organisation. A token that reached it would
//!   outlive the run by years.

use gabriel_core::vars::Redactor;

#[derive(Debug, Clone)]
pub struct RunReport {
    pub collection: String,
    pub environment: Option<String>,
    pub started_ms: u64,
    pub cases: Vec<CaseResult>,
}

#[derive(Debug, Clone)]
pub struct CaseResult {
    /// Request id, e.g. `users/create`.
    pub id: String,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub outcome: CaseOutcome,
    pub assertions: Vec<AssertionLine>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaseOutcome {
    Passed,
    /// The request was sent and an assertion did not hold.
    Failed,
    /// The request could not be sent at all.
    Errored(String),
}

#[derive(Debug, Clone)]
pub struct AssertionLine {
    pub description: String,
    pub passed: bool,
    pub actual: String,
}

impl RunReport {
    pub fn total(&self) -> usize {
        self.cases.len()
    }

    pub fn failures(&self) -> usize {
        self.cases.iter().filter(|c| c.outcome == CaseOutcome::Failed).count()
    }

    pub fn errors(&self) -> usize {
        self.cases.iter().filter(|c| matches!(c.outcome, CaseOutcome::Errored(_))).count()
    }

    pub fn passed(&self) -> usize {
        self.total() - self.failures() - self.errors()
    }

    pub fn duration_ms(&self) -> u64 {
        self.cases.iter().map(|c| c.duration_ms).sum()
    }

    pub fn is_green(&self) -> bool {
        self.failures() == 0 && self.errors() == 0
    }
}

// ── JUnit ───────────────────────────────────────────────────────────────────

/// One `<testsuite>` per request, one `<testcase>` per assertion — the shape
/// Jenkins, GitLab and GitHub Actions all know how to render. A request with no
/// assertions still gets a case, so "it was sent and came back" is visible
/// rather than absent.
pub fn to_junit(report: &RunReport, redactor: &Redactor) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<testsuites name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" time=\"{}\">\n",
        xml(&redactor.apply(&report.collection)),
        report.total(),
        report.failures(),
        report.errors(),
        seconds(report.duration_ms())
    ));

    for case in &report.cases {
        let cases = junit_cases(case, redactor);
        let failures = cases.iter().filter(|c| c.kind == JunitKind::Failure).count();
        let errors = cases.iter().filter(|c| c.kind == JunitKind::Error).count();

        out.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" time=\"{}\">\n",
            xml(&redactor.apply(&case.id)),
            cases.len(),
            failures,
            errors,
            seconds(case.duration_ms)
        ));
        // Properties carry the context a failure needs to be actionable.
        out.push_str("    <properties>\n");
        for (name, value) in [
            ("method", case.method.clone()),
            ("url", redactor.apply(&case.url)),
            ("status", case.status.map(|s| s.to_string()).unwrap_or_default()),
        ] {
            out.push_str(&format!(
                "      <property name=\"{}\" value=\"{}\"/>\n",
                xml(name),
                xml(&value)
            ));
        }
        out.push_str("    </properties>\n");

        for junit_case in cases {
            out.push_str(&format!(
                "    <testcase classname=\"{}\" name=\"{}\" time=\"{}\"",
                xml(&redactor.apply(&case.id)),
                xml(&junit_case.name),
                seconds(case.duration_ms)
            ));
            match junit_case.kind {
                JunitKind::Passed => out.push_str("/>\n"),
                JunitKind::Failure => {
                    out.push_str(">\n");
                    out.push_str(&format!(
                        "      <failure message=\"{}\">{}</failure>\n",
                        xml(&junit_case.message),
                        xml(&junit_case.detail)
                    ));
                    out.push_str("    </testcase>\n");
                }
                JunitKind::Error => {
                    out.push_str(">\n");
                    out.push_str(&format!(
                        "      <error message=\"{}\">{}</error>\n",
                        xml(&junit_case.message),
                        xml(&junit_case.detail)
                    ));
                    out.push_str("    </testcase>\n");
                }
            }
        }
        out.push_str("  </testsuite>\n");
    }

    out.push_str("</testsuites>\n");
    out
}

#[derive(Debug, PartialEq)]
enum JunitKind {
    Passed,
    Failure,
    Error,
}

struct JunitCase {
    name: String,
    kind: JunitKind,
    message: String,
    detail: String,
}

fn junit_cases(case: &CaseResult, redactor: &Redactor) -> Vec<JunitCase> {
    if let CaseOutcome::Errored(reason) = &case.outcome {
        return vec![JunitCase {
            name: format!("{} {}", case.method, redactor.apply(&case.url)),
            kind: JunitKind::Error,
            message: redactor.apply(reason),
            detail: redactor.apply(reason),
        }];
    }

    if case.assertions.is_empty() {
        return vec![JunitCase {
            name: format!(
                "{} {} → {}",
                case.method,
                redactor.apply(&case.url),
                case.status.map(|s| s.to_string()).unwrap_or_else(|| "no response".into())
            ),
            kind: JunitKind::Passed,
            message: String::new(),
            detail: String::new(),
        }];
    }

    case.assertions
        .iter()
        .map(|assertion| JunitCase {
            name: redactor.apply(&assertion.description),
            kind: if assertion.passed { JunitKind::Passed } else { JunitKind::Failure },
            message: format!(
                "expected {}, got {}",
                redactor.apply(&assertion.description),
                redactor.apply(&assertion.actual)
            ),
            detail: format!(
                "{} {}\nstatus: {}\n{}\nactual: {}",
                case.method,
                redactor.apply(&case.url),
                case.status.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
                redactor.apply(&assertion.description),
                redactor.apply(&assertion.actual)
            ),
        })
        .collect()
}

/// JUnit `time` is seconds with a decimal point.
fn seconds(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

/// Escape for both XML text and attribute values.
///
/// Attributes are quoted with `"` so that must go; `'` is escaped too because
/// the same function is used in both positions and being conservative costs
/// nothing. Control characters that XML 1.0 forbids outright are dropped — a
/// response body containing a NUL would otherwise produce a file no parser
/// accepts.
fn xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(ch),
            // XML 1.0 §2.2 permits no other C0 control.
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

// ── HTML ────────────────────────────────────────────────────────────────────

/// A single self-contained file: no scripts, no external stylesheets, no fonts.
///
/// Reports get opened from a CI artifact viewer, often offline and often behind
/// a strict content policy. Anything fetched would simply not appear, and a
/// script would be the wrong thing to ship in a document built from untrusted
/// response data.
pub fn to_html(report: &RunReport, redactor: &Redactor) -> String {
    let status_word = if report.is_green() { "passed" } else { "failed" };
    let mut out = String::new();

    out.push_str(&format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Gabriel run — {title}</title>
<style>
:root {{ color-scheme: light dark; --pass: #1a7f37; --fail: #cf222e; --error: #9a6700; --line: #d0d7de; --muted: #656d76; }}
@media (prefers-color-scheme: dark) {{ :root {{ --line: #30363d; --muted: #8b949e; }} }}
body {{ font: 15px/1.5 ui-sans-serif, -apple-system, system-ui, sans-serif; margin: 0; padding: 2rem 1.5rem; max-width: 60rem; }}
h1 {{ font-size: 1.4rem; margin: 0 0 .25rem; }}
.sub {{ color: var(--muted); margin-bottom: 1.5rem; }}
.totals {{ display: flex; flex-wrap: wrap; gap: 1.5rem; padding: 1rem 0; border-block: 1px solid var(--line); margin-bottom: 1.5rem; }}
.total b {{ display: block; font-size: 1.6rem; font-weight: 600; }}
.total span {{ color: var(--muted); font-size: .85rem; }}
.pass {{ color: var(--pass); }} .fail {{ color: var(--fail); }} .error {{ color: var(--error); }}
.case {{ border: 1px solid var(--line); border-radius: 6px; margin-bottom: .75rem; overflow: hidden; }}
.case > summary {{ padding: .7rem .9rem; cursor: pointer; display: flex; gap: .75rem; align-items: baseline; }}
.case > summary::marker {{ color: var(--muted); }}
.id {{ font-weight: 600; }}
.method {{ font: 12px ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--muted); }}
.url {{ font: 12px ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--muted); overflow-wrap: anywhere; flex: 1; }}
.body {{ padding: 0 .9rem .9rem; }}
ul {{ list-style: none; padding: 0; margin: .5rem 0 0; }}
li {{ padding: .3rem 0; border-top: 1px solid var(--line); font: 13px ui-monospace, SFMono-Regular, Menlo, monospace; overflow-wrap: anywhere; }}
.actual {{ color: var(--muted); }}
.reason {{ font: 13px ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--fail); overflow-wrap: anywhere; }}
footer {{ margin-top: 2rem; color: var(--muted); font-size: .85rem; }}
</style>
</head>
<body>
<h1>{title} — <span class="{status_class}">{status_word}</span></h1>
<div class="sub">{env}{started}</div>
<div class="totals">
  <div class="total"><b>{total}</b><span>requests</span></div>
  <div class="total"><b class="pass">{passed}</b><span>passed</span></div>
  <div class="total"><b class="fail">{failed}</b><span>failed</span></div>
  <div class="total"><b class="error">{errors}</b><span>errored</span></div>
  <div class="total"><b>{duration}</b><span>total</span></div>
</div>
"#,
        title = html(&redactor.apply(&report.collection)),
        status_class = if report.is_green() { "pass" } else { "fail" },
        status_word = status_word,
        env = report
            .environment
            .as_ref()
            .map(|e| format!("environment <b>{}</b> · ", html(e)))
            .unwrap_or_default(),
        started = html(&gabriel_core::format_iso8601(report.started_ms)),
        total = report.total(),
        passed = report.passed(),
        failed = report.failures(),
        errors = report.errors(),
        duration = html(&crate::output::format_duration(report.duration_ms())),
    ));

    for case in &report.cases {
        let (class, label) = match &case.outcome {
            CaseOutcome::Passed => ("pass", "passed"),
            CaseOutcome::Failed => ("fail", "failed"),
            CaseOutcome::Errored(_) => ("error", "errored"),
        };
        // Failures start expanded; nobody opens a report to read the passes.
        let open = if case.outcome == CaseOutcome::Passed { "" } else { " open" };

        out.push_str(&format!(
            "<details class=\"case\"{open}>\n  <summary><span class=\"{class}\">●</span> \
             <span class=\"id\">{id}</span> <span class=\"method\">{method}</span> \
             <span class=\"url\">{url}</span> <span class=\"{class}\">{label}</span></summary>\n  \
             <div class=\"body\">\n",
            open = open,
            class = class,
            id = html(&redactor.apply(&case.id)),
            method = html(&case.method),
            url = html(&redactor.apply(&case.url)),
            label = label,
        ));

        out.push_str(&format!(
            "    <div class=\"actual\">status {} · {}</div>\n",
            case.status.map(|s| s.to_string()).unwrap_or_else(|| "—".into()),
            html(&crate::output::format_duration(case.duration_ms))
        ));

        if let CaseOutcome::Errored(reason) = &case.outcome {
            out.push_str(&format!(
                "    <p class=\"reason\">{}</p>\n",
                html(&redactor.apply(reason))
            ));
        }

        if !case.assertions.is_empty() {
            out.push_str("    <ul>\n");
            for assertion in &case.assertions {
                let mark = if assertion.passed { "<span class=\"pass\">✓</span>" } else { "<span class=\"fail\">✗</span>" };
                out.push_str(&format!(
                    "      <li>{mark} {desc}{actual}</li>\n",
                    mark = mark,
                    desc = html(&redactor.apply(&assertion.description)),
                    actual = if assertion.passed {
                        String::new()
                    } else {
                        format!(
                            " <span class=\"actual\">— got {}</span>",
                            html(&redactor.apply(&assertion.actual))
                        )
                    }
                ));
            }
            out.push_str("    </ul>\n");
        }

        out.push_str("  </div>\n</details>\n");
    }

    out.push_str(&format!(
        "<footer>Generated by gabriel {} · secrets are redacted; response data is escaped, not executed.</footer>\n</body>\n</html>\n",
        env!("CARGO_PKG_VERSION")
    ));
    out
}

/// Escape for HTML text and attribute values alike.
fn html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(id: &str, outcome: CaseOutcome, assertions: Vec<AssertionLine>) -> CaseResult {
        CaseResult {
            id: id.to_string(),
            method: "GET".to_string(),
            url: format!("https://api.test/{id}"),
            status: Some(200),
            duration_ms: 125,
            outcome,
            assertions,
        }
    }

    fn assertion(description: &str, passed: bool) -> AssertionLine {
        AssertionLine {
            description: description.to_string(),
            passed,
            actual: if passed { "200".into() } else { "500".into() },
        }
    }

    fn report() -> RunReport {
        RunReport {
            collection: "demo".to_string(),
            environment: Some("staging".to_string()),
            started_ms: 1_785_283_200_000,
            cases: vec![
                case("users/list", CaseOutcome::Passed, vec![assertion("status == 200", true)]),
                case("users/create", CaseOutcome::Failed, vec![
                    assertion("status == 201", false),
                    assertion("body id exists", true),
                ]),
                case("users/delete", CaseOutcome::Errored("connection refused".into()), vec![]),
            ],
        }
    }

    fn plain() -> Redactor {
        Redactor::default()
    }

    #[test]
    fn the_totals_add_up() {
        let report = report();
        assert_eq!(report.total(), 3);
        assert_eq!(report.passed(), 1);
        assert_eq!(report.failures(), 1);
        assert_eq!(report.errors(), 1);
        assert!(!report.is_green());
    }

    #[test]
    fn junit_reports_counts_at_the_top_level() {
        let xml = to_junit(&report(), &plain());
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains(r#"<testsuites name="demo" tests="3" failures="1" errors="1""#), "{xml}");
    }

    #[test]
    fn junit_distinguishes_a_failed_assertion_from_an_unsendable_request() {
        let xml = to_junit(&report(), &plain());
        // The failed assertion is a <failure>; the transport problem is an <error>.
        assert!(xml.contains("<failure message="), "{xml}");
        assert!(xml.contains("<error message="), "{xml}");
        assert!(xml.contains("connection refused"), "{xml}");
    }

    #[test]
    fn junit_emits_one_case_per_assertion() {
        let xml = to_junit(&report(), &plain());
        assert!(xml.contains(r#"name="status == 200""#), "{xml}");
        assert!(xml.contains(r#"name="status == 201""#), "{xml}");
        assert!(xml.contains(r#"name="body id exists""#), "{xml}");
    }

    #[test]
    fn a_request_without_assertions_still_produces_a_case() {
        let report = RunReport {
            collection: "demo".into(),
            environment: None,
            started_ms: 0,
            cases: vec![case("ping", CaseOutcome::Passed, Vec::new())],
        };
        let xml = to_junit(&report, &plain());
        assert!(xml.contains("<testcase"), "a sent request should be visible:\n{xml}");
        assert!(xml.contains("→ 200"), "{xml}");
    }

    #[test]
    fn junit_time_is_seconds() {
        let xml = to_junit(&report(), &plain());
        // 125 ms per case, three cases.
        assert!(xml.contains(r#"time="0.375""#), "suite total wrong:\n{xml}");
        assert!(xml.contains(r#"time="0.125""#), "case time wrong:\n{xml}");
    }

    /// A response body is server-controlled. Unescaped, it produces XML no CI
    /// parser will accept — which turns a red build into a broken pipeline.
    #[test]
    fn junit_escapes_everything_a_server_can_control() {
        let mut report = report();
        report.cases[1].url = "https://api.test/?q=<script>&x='\"".into();
        report.cases[1].assertions[0].actual = "<!--\"bad\" & 'worse'-->".into();

        let xml = to_junit(&report, &plain());

        // No raw markup survived.
        assert!(!xml.contains("<script>"), "{xml}");
        assert!(!xml.contains("<!--"), "{xml}");
        assert!(xml.contains("&lt;script&gt;"), "{xml}");
        assert!(xml.contains("&amp;"), "{xml}");
        assert!(xml.contains("&quot;") && xml.contains("&apos;"), "{xml}");
    }

    #[test]
    fn junit_drops_control_characters_xml_forbids() {
        let mut report = report();
        report.cases[0].id = "weird\u{0}\u{1}\u{1b}name".into();
        let xml = to_junit(&report, &plain());

        assert!(!xml.contains('\u{0}') && !xml.contains('\u{1b}'), "control bytes survived");
        assert!(xml.contains("weirdname"), "{xml}");
        // Tabs and newlines are legal and should be kept.
        assert_eq!(super::xml("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn secrets_never_reach_a_report_artifact() {
        let mut report = report();
        report.cases[0].url = "https://api.test/?token=sk-live-SECRET".into();
        report.cases[1].assertions[0].actual = "sk-live-SECRET".into();
        report.cases[2].outcome = CaseOutcome::Errored("auth failed for sk-live-SECRET".into());

        let redactor = Redactor::new(vec!["sk-live-SECRET".to_string()]);
        let xml = to_junit(&report, &redactor);
        let page = to_html(&report, &redactor);

        assert!(!xml.contains("sk-live-SECRET"), "JUnit leaked a secret:\n{xml}");
        assert!(!page.contains("sk-live-SECRET"), "HTML leaked a secret:\n{page}");
    }

    #[test]
    fn html_is_self_contained() {
        let page = to_html(&report(), &plain());
        assert!(page.starts_with("<!doctype html>"));
        // Nothing fetched, nothing executed.
        assert!(!page.contains("<script"), "a report should not carry script");
        assert!(!page.contains("http://") && !page.to_lowercase().contains("src="), "{page}");
        assert!(page.contains("</html>"));
    }

    /// The stored-XSS case: a report is opened in a browser by whoever reads the
    /// CI artifact.
    #[test]
    fn html_escapes_server_controlled_data() {
        let mut report = report();
        report.cases[1].url = "https://api.test/<script>alert(1)</script>".into();
        report.cases[1].assertions[0].actual =
            r#"<img src=x onerror="alert(1)">"#.into();
        report.cases[2].outcome = CaseOutcome::Errored("<b>bold</b>".into());

        let page = to_html(&report, &plain());

        assert!(!page.contains("<script>alert"), "script tag survived:\n{page}");
        assert!(!page.contains("onerror=\"alert"), "event handler survived:\n{page}");
        assert!(!page.contains("<b>bold</b>"), "markup survived:\n{page}");
        assert!(page.contains("&lt;script&gt;"), "{page}");
    }

    #[test]
    fn html_shows_the_summary_and_marks_failures() {
        let page = to_html(&report(), &plain());
        assert!(page.contains("demo — <span class=\"pass\">passed</span>") == false);
        assert!(page.contains(">failed<"), "the run should read as failed:\n{page}");
        assert!(page.contains("staging"), "environment should be shown");
        // Failures are expanded; passes are not.
        assert!(page.contains("<details class=\"case\" open>"), "{page}");
        assert!(page.contains("connection refused"));
    }

    #[test]
    fn a_green_run_says_so() {
        let report = RunReport {
            collection: "demo".into(),
            environment: None,
            started_ms: 0,
            cases: vec![case("ok", CaseOutcome::Passed, vec![assertion("status == 200", true)])],
        };
        assert!(report.is_green());
        let page = to_html(&report, &plain());
        assert!(page.contains("passed"), "{page}");
        let xml = to_junit(&report, &plain());
        assert!(xml.contains(r#"failures="0" errors="0""#), "{xml}");
    }

    #[test]
    fn an_empty_run_produces_valid_output_rather_than_nothing() {
        let report = RunReport {
            collection: "empty".into(),
            environment: None,
            started_ms: 0,
            cases: Vec::new(),
        };
        let xml = to_junit(&report, &plain());
        assert!(xml.contains(r#"tests="0""#) && xml.contains("</testsuites>"), "{xml}");
        assert!(to_html(&report, &plain()).contains("</html>"));
    }
}
