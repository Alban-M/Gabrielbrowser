//! Performance harness for Gabriel.
//!
//! Everything is measured against a local origin server on loopback, so the
//! numbers describe Gabriel rather than somebody's network. Each section
//! reports a baseline alongside Gabriel's own figure — an absolute latency is
//! not interesting; the overhead Gabriel adds to it is.
//!
//! Run with `cargo run --release -p gabriel-bench`. Debug builds are pointless
//! here: they exaggerate the crypto and parsing paths by an order of magnitude.

mod server;
mod stats;

use gabriel_core::capture::{Capture, CapturedBody, CapturedRequest, CapturedResponse};
use gabriel_core::model::{
    AssertOp, AssertTarget, Assertion, CaptureSource, FieldMap, RequestSpec, VarCapture,
};
use gabriel_core::vars::Resolver;
use gabriel_engine::session::SessionStore;
use gabriel_engine::{Executor, RunContext};
use gabriel_proxy::store::{CaptureFilter, CaptureStore};
use gabriel_proxy::{Proxy, ProxyConfig};
use gabriel_vault::{KeySource, Vault};
use stats::{Samples, print_row, print_table, rate};
use std::path::PathBuf;
use std::time::Instant;

/// Enough samples for a stable p95 without making the run tedious.
const HTTP_ITERATIONS: usize = 400;
const WARMUP: usize = 40;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if cfg!(debug_assertions) {
        eprintln!("warning: this is a debug build; the numbers will be misleading");
    }

    println!("Gabriel performance report");
    println!("{}", "=".repeat(96));
    println!(
        "host: {} · {} logical cores · loopback origin server",
        std::env::consts::OS,
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
    );

    let addr = server::spawn().await;
    let origin = format!("http://{addr}");

    startup_section();
    request_path_section(&origin).await;
    body_size_section(&origin).await;
    proxy_section(&origin).await;
    tls_section();
    capture_store_section();
    core_section();
    vault_section();

    println!("\n{}", "=".repeat(96));
}

/// How long the binary takes to be useful. Bruno's sub-second start is a stated
/// reason developers left Electron clients; a CLI has no excuse to be slower.
fn startup_section() {
    let Some(binary) = gabriel_binary() else {
        println!("\nStartup — skipped (no `gabriel` binary beside this one)");
        return;
    };

    let dir = temp_dir("startup");
    let collection = gabriel_collection::Collection::init(&dir, "bench").expect("init");
    let mut collection =
        gabriel_collection::Collection::load(collection.root()).expect("load");
    for i in 0..50 {
        let mut spec = RequestSpec::new("GET", "https://api.test/{{id}}");
        spec.name = Some(format!("request {i}"));
        collection.save_request(&format!("group/request-{i}"), &spec).expect("save");
    }

    let mut version = Samples::new("gabriel --version");
    let mut list = Samples::new("gabriel ls (51 requests)");

    for i in 0..30 {
        let started = Instant::now();
        let _ = std::process::Command::new(&binary).arg("--version").output().expect("run");
        if i >= 5 {
            version.push(started.elapsed());
        }

        let started = Instant::now();
        let _ = std::process::Command::new(&binary)
            .arg("--dir")
            .arg(&dir)
            .arg("ls")
            .output()
            .expect("run");
        if i >= 5 {
            list.push(started.elapsed());
        }
    }

    print_table(
        "Process startup (fork/exec included — that is most of it)",
        &[version.summary(), list.summary()],
    );

    if let Ok(meta) = std::fs::metadata(&binary) {
        print_row("binary size", gabriel_core::format_bytes(meta.len() as usize));
    }
}

/// The number that matters most: what does Gabriel's request layer add on top
/// of the HTTP client it is built on?
async fn request_path_section(origin: &str) {
    let url = format!("{origin}/json");

    // Baseline: the same client, driven directly, with no Gabriel in the path.
    let client = reqwest::Client::builder().build().expect("client");
    let mut baseline = Samples::new("reqwest direct (baseline)");
    for i in 0..(HTTP_ITERATIONS + WARMUP) {
        let started = Instant::now();
        let response = client.get(&url).send().await.expect("send");
        let _ = response.bytes().await.expect("body");
        if i >= WARMUP {
            baseline.push(started.elapsed());
        }
    }

    let mut plain = Samples::new("gabriel engine");
    run_engine_n(&RequestSpec::new("GET", &url), &mut plain, HTTP_ITERATIONS).await;

    // The same request with everything the format allows switched on.
    let mut spec = RequestSpec::new("GET", "{{base}}/json");
    spec.headers.set("Accept", "application/json");
    spec.headers.set("X-Request-Id", "{{$uuid}}");
    spec.captures.push(VarCapture {
        var: "count".into(),
        from: CaptureSource::Body,
        path: Some("meta.count".into()),
    });
    spec.asserts.push(Assertion {
        target: AssertTarget::Status,
        path: None,
        op: AssertOp::Eq,
        value: Some(toml::Value::Integer(200)),
    });
    spec.asserts.push(Assertion {
        target: AssertTarget::Body,
        path: Some("meta.ok".into()),
        op: AssertOp::Eq,
        value: Some(toml::Value::Boolean(true)),
    });
    let mut loaded = Samples::new("gabriel engine + vars/captures/asserts");
    run_engine_with_vars(&spec, origin, &mut loaded).await;

    print_table(
        "Request path · 70-byte JSON response over loopback",
        &[baseline.summary(), plain.summary(), loaded.summary()],
    );

    let overhead = plain.summary().p50_ms - baseline.summary().p50_ms;
    let full_overhead = loaded.summary().p50_ms - baseline.summary().p50_ms;
    print_row("engine overhead at p50", stats::ms(overhead.max(0.0)));
    print_row("…with templates + asserts", stats::ms(full_overhead.max(0.0)));
}

/// Bodies are buffered rather than streamed, so this is where that shows.
async fn body_size_section(origin: &str) {
    let mut rows = Vec::new();
    for size in [1_024usize, 100 * 1024, 1024 * 1024, 8 * 1024 * 1024] {
        let url = format!("{origin}/bytes/{size}");
        let mut samples =
            Samples::new(format!("gabriel engine · {}", gabriel_core::format_bytes(size)));
        let iterations = if size > 1024 * 1024 { 40 } else { 200 };
        run_engine_n(&RequestSpec::new("GET", &url), &mut samples, iterations).await;
        rows.push(samples.summary());
    }
    print_table("Response size · body fully buffered and read", &rows);
}

/// What the capture proxy costs per request: forwarding, buffering both bodies,
/// and appending a line to the capture log.
async fn proxy_section(origin: &str) {
    let dir = temp_dir("proxy");
    let store = CaptureStore::new(dir.join("captures.ndjson"));
    let config = ProxyConfig { addr: ([127, 0, 0, 1], 0).into(), ..Default::default() };
    let proxy = Proxy::new(config, &dir, store, SessionStore::new()).expect("proxy");
    let running = proxy.start().await.expect("start");
    let proxy_url = format!("http://{}", running.addr);

    let url = format!("{origin}/json");
    let mut direct = Samples::new("gabriel engine, direct");
    run_engine_n(&RequestSpec::new("GET", &url), &mut direct, HTTP_ITERATIONS).await;

    let mut through = RequestSpec::new("GET", &url);
    through.settings.proxy = Some(proxy_url.clone());
    let mut proxied = Samples::new("gabriel engine, via capture proxy");
    run_engine_n(&through, &mut proxied, HTTP_ITERATIONS).await;

    // Throughput with concurrency, which is how a page load actually arrives.
    let concurrent_direct = throughput(&url, None, 8, 100).await;
    let concurrent_proxied = throughput(&url, Some(&proxy_url), 8, 100).await;

    print_table(
        "Capture proxy overhead · HTTP, small JSON",
        &[direct.summary(), proxied.summary()],
    );
    print_row(
        "added per request at p50",
        stats::ms((proxied.summary().p50_ms - direct.summary().p50_ms).max(0.0)),
    );
    print_row("throughput direct · 8 concurrent", concurrent_direct);
    print_row("throughput via proxy · 8 concurrent", concurrent_proxied);
    print_row(
        "captures written",
        CaptureStore::new(dir.join("captures.ndjson"))
            .count()
            .unwrap_or(0)
            .to_string(),
    );

    running.shutdown().await;
}

/// Minting a leaf certificate is the one-time cost of intercepting a new host;
/// it happens on the first HTTPS request to each origin.
fn tls_section() {
    let dir = temp_dir("tls");
    let ca = gabriel_proxy::ca::CertificateAuthority::load_or_create(&dir).expect("ca");

    let mut mint = Samples::new("mint leaf cert · new host");
    for i in 0..60 {
        let host = format!("host-{i}.bench.test");
        let started = Instant::now();
        ca.server_config(&host).expect("config");
        mint.push(started.elapsed());
    }

    let mut cached = Samples::new("cached cert · repeat host");
    for _ in 0..2000 {
        let started = Instant::now();
        ca.server_config("host-0.bench.test").expect("config");
        cached.push(started.elapsed());
    }

    let started = Instant::now();
    let _ = gabriel_proxy::ca::CertificateAuthority::load_or_create(temp_dir("tls-new"));
    let ca_generation = started.elapsed();

    print_table("TLS interception", &[mint.summary(), cached.summary()]);
    print_row(
        "CA generation · once per install",
        stats::ms(ca_generation.as_secs_f64() * 1000.0),
    );
}

/// The capture log is newline-delimited JSON, and reads parse the whole file.
/// This section exists to find out where that stops being acceptable.
fn capture_store_section() {
    let mut append = Samples::new("append one capture");
    let dir = temp_dir("store");
    let store = CaptureStore::new(dir.join("captures.ndjson"));

    for i in 0..2000 {
        let capture = sample_capture(i);
        let started = Instant::now();
        store.append(&capture).expect("append");
        append.push(started.elapsed());
    }
    print_table("Capture log writes", &[append.summary()]);

    let mut list_rows = Vec::new();
    let mut get_rows = Vec::new();
    for size in [100usize, 1_000, 10_000, 50_000] {
        let dir = temp_dir(&format!("store-{size}"));
        let store = CaptureStore::new(dir.join("captures.ndjson"));
        for i in 0..size {
            store.append(&sample_capture(i)).expect("append");
        }
        let bytes = std::fs::metadata(store.path()).map(|m| m.len()).unwrap_or(0);

        let mut list = Samples::new(format!(
            "ls --limit 30 · {size} captures ({})",
            gabriel_core::format_bytes(bytes as usize)
        ));
        for _ in 0..10 {
            let started = Instant::now();
            store.list(&CaptureFilter::default(), 30).expect("list");
            list.push(started.elapsed());
        }
        list_rows.push(list.summary());

        let mut get = Samples::new(format!("get by id · {size} captures"));
        for _ in 0..10 {
            let started = Instant::now();
            store.get(&format!("cap-{}", size / 2)).expect("get");
            get.push(started.elapsed());
        }
        get_rows.push(get.summary());
    }
    print_table("Capture log reads · every read parses the whole file", &list_rows);
    print_table("Capture lookup by id", &get_rows);
    attribution_section();
}

/// Where the two slow paths actually spend their time, so that a fix can be
/// aimed rather than guessed at.
fn attribution_section() {
    use std::io::Write as _;

    println!("\nBottleneck attribution");

    // Writes: the store opens, writes and closes the file for every capture.
    let dir = temp_dir("attr-write");
    let path = dir.join("reopen.ndjson");
    let line = serde_json::to_string(&sample_capture(1)).expect("json");

    let iterations = 2000;
    let started = Instant::now();
    for _ in 0..iterations {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open");
        writeln!(file, "{line}").expect("write");
    }
    let reopen = started.elapsed();

    let path = dir.join("persistent.ndjson");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("open");
    let started = Instant::now();
    for _ in 0..iterations {
        writeln!(file, "{line}").expect("write");
    }
    let persistent = started.elapsed();

    print_row(
        "append · reopen per capture (current)",
        format!(
            "{} · {}",
            stats::ms(reopen.as_secs_f64() * 1000.0 / iterations as f64),
            rate(iterations, reopen)
        ),
    );
    print_row(
        "append · one held handle",
        format!(
            "{} · {}",
            stats::ms(persistent.as_secs_f64() * 1000.0 / iterations as f64),
            rate(iterations, persistent)
        ),
    );

    // Reads: is the cost the disk, or the JSON?
    let dir = temp_dir("attr-read");
    let store = CaptureStore::new(dir.join("captures.ndjson"));
    for i in 0..10_000 {
        store.append(&sample_capture(i)).expect("append");
    }

    let started = Instant::now();
    let text = std::fs::read_to_string(store.path()).expect("read");
    let read_elapsed = started.elapsed();

    let started = Instant::now();
    let parsed = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Capture>(line).ok())
        .count();
    let parse_elapsed = started.elapsed();

    print_row(
        "read 6 MB log from page cache",
        stats::ms(read_elapsed.as_secs_f64() * 1000.0),
    );
    print_row(
        &format!("deserialize {parsed} captures"),
        stats::ms(parse_elapsed.as_secs_f64() * 1000.0),
    );
    println!(
        "  → the cost is deserialization, not I/O; `ls --limit 30` decodes every\n    \
         capture ever recorded to show the last thirty."
    );
}

/// The pure-CPU paths: templates, JSON addressing, and response comparison.
fn core_section() {
    println!("\nCore CPU paths");

    let mut resolver = Resolver::new().with_vars(
        [
            ("base_url".to_string(), "https://api.example.com".to_string()),
            ("version".to_string(), "v2".to_string()),
            ("user_id".to_string(), "u_12345".to_string()),
        ]
        .into(),
    );

    let template = "{{base_url}}/{{version}}/users/{{user_id}}/orders?trace={{$uuid}}";
    let iterations = 200_000;
    let started = Instant::now();
    for _ in 0..iterations {
        resolver.resolve(template).expect("resolve");
    }
    let template_elapsed = started.elapsed();

    let plain = "https://api.example.com/v2/users/u_12345/orders";
    let started = Instant::now();
    for _ in 0..iterations {
        resolver.resolve(plain).expect("resolve");
    }
    let plain_elapsed = started.elapsed();

    print_row(
        "resolve template · 4 substitutions",
        format!(
            "{} · {}",
            rate(iterations, template_elapsed),
            stats::ms(template_elapsed.as_secs_f64() * 1000.0 / iterations as f64)
        ),
    );
    print_row("resolve string with no template", rate(iterations, plain_elapsed));

    for items in [100usize, 10_000] {
        let document = sample_json(items);
        let text = serde_json::to_string(&document).expect("json");
        let label = gabriel_core::format_bytes(text.len());

        let started = Instant::now();
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("parse");
        let parse_elapsed = started.elapsed();

        let started = Instant::now();
        for _ in 0..1000 {
            gabriel_core::jsonpath::select(&parsed, "items[50].name").expect("select");
        }
        let select_elapsed = started.elapsed();

        let mut changed = parsed.clone();
        changed["items"][items / 2]["name"] = serde_json::Value::String("changed".into());
        let started = Instant::now();
        let changes = gabriel_core::diff::diff_json(&parsed, &changed);
        let diff_elapsed = started.elapsed();

        println!("  {label} document · {items} items");
        print_row("    parse", stats::ms(parse_elapsed.as_secs_f64() * 1000.0));
        print_row("    jsonpath select", rate(1000, select_elapsed));
        print_row(
            "    structural diff",
            format!(
                "{} · {} change(s) found",
                stats::ms(diff_elapsed.as_secs_f64() * 1000.0),
                changes.len()
            ),
        );
    }

    // Cookie matching runs on every request that inherits a session.
    let mut sessions = SessionStore::new();
    for i in 0..200 {
        sessions.record_set_cookies(
            "bench",
            [format!("c{i}=value{i}; Path=/").as_str()],
            "api.example.com",
            "/",
        );
    }
    let iterations = 100_000;
    let started = Instant::now();
    for _ in 0..iterations {
        std::hint::black_box(sessions.cookie_header("bench", "api.example.com", "/v2/users", true));
    }
    print_row(
        "build Cookie header · 200 cookies",
        rate(iterations, started.elapsed()),
    );
}

/// Argon2id is deliberately slow. This confirms it is slow in the right place —
/// once, on unlock — and that everything after it is free.
fn vault_section() {
    let dir = temp_dir("vault");
    let path = dir.join("vault.json");
    let source = KeySource::Passphrase("bench-passphrase-abcdef".to_string());

    let started = Instant::now();
    let mut vault = Vault::open(&path, &source).expect("create");
    let create = started.elapsed();

    for i in 0..100 {
        vault.set(format!("secret_{i}"), format!("value-{i}-abcdefghijklmnop"));
    }
    let started = Instant::now();
    vault.save().expect("save");
    let save = started.elapsed();

    let started = Instant::now();
    let vault = Vault::open(&path, &source).expect("open");
    let open = started.elapsed();

    let iterations = 100_000;
    let started = Instant::now();
    for _ in 0..iterations {
        std::hint::black_box(vault.get("secret_50"));
    }
    let lookup = started.elapsed();

    println!("\nVault · Argon2id, 64 MiB / 3 passes");
    print_row("create · derive key + write", stats::ms(create.as_secs_f64() * 1000.0));
    print_row("unlock · derive key + decrypt", stats::ms(open.as_secs_f64() * 1000.0));
    print_row("save 100 secrets", stats::ms(save.as_secs_f64() * 1000.0));
    print_row("lookup once unlocked", rate(iterations, lookup));
}

// ── helpers ─────────────────────────────────────────────────────────────────

async fn run_engine_n(spec: &RequestSpec, samples: &mut Samples, iterations: usize) {
    let mut executor = Executor::new();
    let mut resolver = Resolver::new();
    let mut sessions = SessionStore::new();

    for i in 0..(iterations + WARMUP) {
        let started = Instant::now();
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);
        executor.execute(spec, &mut ctx).await.expect("execute");
        if i >= WARMUP {
            samples.push(started.elapsed());
        }
    }
}

async fn run_engine_with_vars(spec: &RequestSpec, origin: &str, samples: &mut Samples) {
    let mut executor = Executor::new();
    let mut resolver = Resolver::new().with_vars([("base".to_string(), origin.to_string())].into());
    let mut sessions = SessionStore::new();

    for i in 0..(HTTP_ITERATIONS + WARMUP) {
        let started = Instant::now();
        let mut ctx = RunContext::new(&mut resolver, &mut sessions);
        executor.execute(spec, &mut ctx).await.expect("execute");
        if i >= WARMUP {
            samples.push(started.elapsed());
        }
    }
}

async fn throughput(url: &str, proxy: Option<&str>, workers: usize, per_worker: usize) -> String {
    let mut builder = reqwest::Client::builder();
    if let Some(proxy) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy).expect("proxy"));
    }
    let client = builder.build().expect("client");

    let started = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..workers {
        let client = client.clone();
        let url = url.to_string();
        handles.push(tokio::spawn(async move {
            for _ in 0..per_worker {
                let response = client.get(&url).send().await.expect("send");
                let _ = response.bytes().await.expect("body");
            }
        }));
    }
    for handle in handles {
        handle.await.expect("worker");
    }
    rate(workers * per_worker, started.elapsed())
}

fn sample_capture(index: usize) -> Capture {
    let mut headers = FieldMap::default();
    headers.set("Accept", "application/json");
    headers.set("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)");
    headers.set("Cookie", "session_id=abc123; theme=dark");

    Capture {
        id: format!("cap-{index}"),
        at: 1_785_000_000_000 + index as u64,
        duration_ms: 12,
        session: Some("bench".into()),
        page: Some("https://app.example.com/dashboard".into()),
        request: CapturedRequest {
            method: "GET".into(),
            url: format!("https://api.example.com/v2/items/{index}"),
            http_version: "HTTP/2".into(),
            headers: headers.clone(),
            body: None,
        },
        response: Some(CapturedResponse {
            status: 200,
            status_text: "OK".into(),
            headers,
            body: Some(CapturedBody::Text {
                text: format!(r#"{{"id":{index},"name":"item {index}","ok":true}}"#),
            }),
        }),
    }
}

fn sample_json(items: usize) -> serde_json::Value {
    let list: Vec<serde_json::Value> = (0..items)
        .map(|i| {
            serde_json::json!({
                "id": i,
                "name": format!("item {i}"),
                "sku": format!("SKU-{i:08}"),
                "price": 19.99,
                "tags": ["alpha", "beta"],
                "meta": { "active": true, "updated": "2026-07-29T00:00:00Z" }
            })
        })
        .collect();
    serde_json::json!({ "items": list, "total": items })
}

fn gabriel_binary() -> Option<PathBuf> {
    let candidate = std::env::current_exe().ok()?.parent()?.join("gabriel");
    candidate.exists().then_some(candidate)
}

fn temp_dir(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "gabriel-bench-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}
