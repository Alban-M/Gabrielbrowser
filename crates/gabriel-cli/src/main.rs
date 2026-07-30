//! `gabriel` — the command line over the workbench.
//!
//! The loop this exists to make fast:
//!
//! ```text
//! gabriel capture start          # browse; every request is recorded
//! gabriel capture ls             # find the one you care about
//! gabriel promote <id>           # it becomes an editable file, session and all
//! gabriel run <name>             # replay it, still authenticated
//! ```

mod codegen;
mod output;
mod report;
mod support;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use gabriel_collection::Collection;
use gabriel_core::capture::PromoteOptions;
use gabriel_core::vars::{Redactor, Resolver};
use gabriel_engine::session::SessionStore;
use gabriel_engine::{Executor, RunContext};
use gabriel_proxy::store::{CaptureFilter, CaptureStore};
use gabriel_proxy::{Proxy, ProxyConfig};
use gabriel_vault::{KeySource, Vault};
use output::Style;
use std::path::{Path, PathBuf};
use support::LazySecrets;

#[derive(Parser)]
#[command(
    name = "gabriel",
    version,
    about = "A local-first API workbench: capture live traffic, promote it to editable requests, replay it with the session intact.",
    max_term_width = 100
)]
struct Cli {
    /// Directory to start looking for a collection from.
    #[arg(long, global = true, value_name = "DIR")]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a collection in the current directory.
    Init {
        /// Collection name. Defaults to the directory name.
        name: Option<String>,
    },

    /// List the requests in the collection.
    Ls,

    /// Create a request file.
    New {
        /// Path within the collection, e.g. `users/create`.
        path: String,
        #[arg(short = 'X', long, default_value = "GET")]
        method: String,
        url: String,
    },

    /// Send a request and show the response.
    Run(RunArgs),

    /// Record traffic through the local proxy.
    #[command(subcommand)]
    Capture(CaptureCommand),

    /// Turn a capture into an editable request file.
    Promote(PromoteArgs),

    /// Compare two captured responses field by field.
    Diff {
        /// Capture id (a unique prefix will do).
        before: String,
        after: String,
    },

    /// Manage the encrypted secret store.
    #[command(subcommand)]
    Vault(VaultCommand),

    /// Manage captured browser sessions.
    #[command(subcommand)]
    Session(SessionCommand),

    /// Open a WebSocket, send frames, and watch what comes back.
    Ws {
        /// Request name from the collection, or a ws:// / wss:// / https:// URL.
        target: String,
        /// Text frame to send once connected. Repeatable, sent in order.
        #[arg(short, long = "send", value_name = "TEXT")]
        send: Vec<String>,
        /// Stop after this many messages (pings do not count).
        #[arg(short, long, default_value_t = 50)]
        messages: usize,
        /// Stop listening after this many seconds.
        #[arg(short, long, default_value_t = 30)]
        timeout: u64,
        /// Close as soon as the frames are sent, without waiting.
        #[arg(long)]
        close_after_send: bool,
        /// Subprotocol to request. Repeatable.
        #[arg(long = "subprotocol", value_name = "NAME")]
        subprotocols: Vec<String>,
        #[arg(short, long)]
        env: Option<String>,
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
        #[arg(short = 'S', long)]
        session: Option<String>,
    },

    /// Import or export captured traffic as HAR.
    #[command(subcommand)]
    Har(HarCommand),

    /// Decode and inspect a JWT locally, without sending it anywhere.
    Jwt {
        /// The token, `-` to read stdin, or omitted with --capture.
        token: Option<String>,
        /// Pull the token out of a capture's Authorization header or body.
        #[arg(long, value_name = "ID")]
        capture: Option<String>,
        /// Print the decoded header and payload as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Print a request as a curl command.
    Curl {
        /// Request name, id, or a unique suffix.
        request: String,
        #[arg(short, long)]
        env: Option<String>,
        /// Override a variable: `--var user_id=7`. Repeatable.
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
        /// Session to take cookies from for `auth = "session"`.
        #[arg(short, long)]
        session: Option<String>,
        /// Include credentials instead of masking them.
        #[arg(long)]
        show_secrets: bool,
        /// Print on one line instead of wrapping with continuations.
        #[arg(long)]
        one_line: bool,
    },

    /// List environments.
    Env,

    /// Show the interception CA and how to trust it.
    Ca {
        /// Print the certificate itself rather than instructions.
        #[arg(long)]
        pem: bool,
    },
}

#[derive(Args)]
struct RunArgs {
    /// Request name, id, or a unique suffix of one.
    #[arg(required_unless_present = "all")]
    request: Option<String>,

    /// Run every request in the collection, in order, sharing captured
    /// variables between them.
    #[arg(long)]
    all: bool,

    /// Environment to resolve variables from.
    #[arg(short, long)]
    env: Option<String>,

    /// Override a variable: `--var user_id=7`. Repeatable.
    #[arg(long = "var", value_name = "KEY=VALUE")]
    vars: Vec<String>,

    /// Session whose cookies `auth = "session"` should use.
    #[arg(short, long)]
    session: Option<String>,

    /// Show request headers, response headers, and the request body.
    #[arg(short, long)]
    verbose: bool,

    /// Print the response body only — for piping into `jq`.
    #[arg(long, conflicts_with = "verbose")]
    quiet: bool,

    /// Allow `{{env:NAME}}` to read the process environment.
    #[arg(long)]
    allow_env: bool,

    /// Print secrets instead of masking them.
    #[arg(long)]
    show_secrets: bool,

    /// Truncate the printed body at this many characters.
    #[arg(long, default_value_t = 4000)]
    body_limit: usize,

    /// Follow the response as a server-sent event stream, printing events as
    /// they arrive.
    #[arg(long, conflicts_with = "all")]
    stream: bool,

    /// Stop after this many events.
    #[arg(long, default_value_t = 100, requires = "stream")]
    events: usize,

    /// Stop following after this many seconds.
    #[arg(long, default_value_t = 30, requires = "stream")]
    stream_timeout: u64,

    /// Write a JUnit XML report, for CI to render.
    #[arg(long, value_name = "FILE")]
    junit: Option<PathBuf>,

    /// Write a self-contained HTML report, for people to read.
    #[arg(long, value_name = "FILE")]
    html: Option<PathBuf>,
}

#[derive(Subcommand)]
enum CaptureCommand {
    /// Start the proxy and record until interrupted.
    Start {
        #[arg(short, long, default_value_t = 8888)]
        port: u16,
        /// Session name to file captured cookies under.
        #[arg(short, long, default_value = "default")]
        session: String,
        /// Host to tunnel without decrypting. Repeatable.
        #[arg(long = "exclude", value_name = "HOST")]
        exclude: Vec<String>,
        /// Intercept only these hosts. Repeatable.
        #[arg(long = "only", value_name = "HOST")]
        only: Vec<String>,
    },
    /// List captured requests, most recent first.
    Ls {
        /// Substring match against the host, e.g. `--host api.example.com`.
        #[arg(long)]
        host: Option<String>,
        /// Substring match against the whole URL, e.g. `--url /v2/orders`.
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        method: Option<String>,
        /// Only show responses at or above this status, e.g. `--status 400`.
        #[arg(long)]
        status: Option<u16>,
        /// Only show responses at or below this status. Combine with `--status`
        /// for a range: `--status 400 --status-max 499`.
        #[arg(long)]
        status_max: Option<u16>,
        /// Only show captures recorded under this session.
        #[arg(long)]
        session: Option<String>,
        #[arg(short, long, default_value_t = 30)]
        limit: usize,
    },
    /// Show one capture in full.
    Show {
        /// Capture id, or a unique prefix.
        id: String,
    },
    /// Delete the capture log.
    Clear,
}

#[derive(Args)]
struct PromoteArgs {
    /// Capture id, or a unique prefix.
    id: String,

    /// Where to save it, e.g. `users/create`. Defaults to a name derived from
    /// the URL.
    #[arg(long)]
    to: Option<String>,

    /// Write the captured `Cookie` header into the file instead of referring to
    /// the session. Off by default — collections get committed.
    #[arg(long)]
    inline_cookies: bool,

    /// Write a captured bearer token into the file instead of the vault. Off by
    /// default, for the same reason.
    #[arg(long)]
    inline_token: bool,

    /// Also record the request's origin as `base_url` in this environment.
    #[arg(short, long)]
    env: Option<String>,

    /// Replace an existing request file. Without this, promoting onto a path
    /// that already exists stops rather than discarding what is there.
    #[arg(long)]
    force: bool,
}

#[derive(Subcommand)]
enum HarCommand {
    /// Write captures to a HAR file that other tools can read.
    Export {
        /// Where to write. Omit for stdout.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        method: Option<String>,
        #[arg(long)]
        status: Option<u16>,
        #[arg(long)]
        session: Option<String>,
        /// How many captures to include, newest first.
        #[arg(short, long, default_value_t = 1000)]
        limit: usize,
    },
    /// Read a HAR file from DevTools, Charles, Proxyman or Firefox into the
    /// capture log, where its requests can be promoted and replayed.
    Import {
        file: PathBuf,
        /// File the imported captures under this session name.
        #[arg(short, long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
enum VaultCommand {
    /// Store a secret.
    Set { name: String, value: String },
    /// List secret names. Values are never printed.
    Ls,
    /// Remove a secret.
    Rm { name: String },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// List sessions and how many cookies each holds.
    Ls,
    /// Forget a session's cookies.
    Clear { name: String },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let style = Style::detect();

    match run(cli, &style) {
        Ok(Outcome::Success) => std::process::ExitCode::SUCCESS,
        // A failed assertion is a result, not a crash: exit 1, no error banner.
        Ok(Outcome::AssertionsFailed) => std::process::ExitCode::from(1),
        Err(error) => {
            eprintln!("{} {error:#}", style.red("error:"));
            std::process::ExitCode::from(2)
        }
    }
}

enum Outcome {
    Success,
    AssertionsFailed,
}

fn run(cli: Cli, style: &Style) -> Result<Outcome> {
    let start_dir = cli.dir.unwrap_or_else(|| PathBuf::from("."));

    match cli.command {
        Command::Init { name } => {
            let name = name.unwrap_or_else(|| support::directory_name(&start_dir));
            let collection = Collection::init(&start_dir, &name)?;
            println!("{} {}", style.green("created"), collection.root().display());
            if let Some(warning) = gabriel_core::permission_warning() {
                eprintln!("{} {warning}", style.yellow("warning:"));
            }
            println!();
            println!("  {}", style.dim("gabriel run example        send the starter request"));
            println!("  {}", style.dim("gabriel capture start      record live traffic"));
            Ok(Outcome::Success)
        }

        Command::Ls => {
            let collection = open_collection(&start_dir)?;
            if collection.requests().is_empty() {
                println!(
                    "{}",
                    style.dim("no requests yet — try `gabriel new users/list https://…`")
                );
            }
            for entry in collection.requests() {
                println!(
                    "{:<28} {:<6} {}",
                    style.bold(&entry.id),
                    entry.spec.method,
                    style.dim(&entry.spec.url)
                );
            }
            // Broken files are listed too — silently omitting them would make
            // a request look deleted rather than malformed.
            for problem in collection.problems() {
                println!(
                    "{:<28} {}",
                    style.red(&problem.id),
                    style.dim(&format!("unreadable: {}", problem.message.lines().next().unwrap_or("")))
                );
            }
            if !collection.problems().is_empty() {
                eprintln!(
                    "{} {} request file(s) could not be read",
                    style.yellow("warning:"),
                    collection.problems().len()
                );
            }
            Ok(Outcome::Success)
        }

        Command::New { path, method, url } => {
            let mut collection = open_collection(&start_dir)?;
            let spec = gabriel_core::RequestSpec::new(&method, &url);
            let written = collection.save_request(&path, &spec)?;
            println!("{} {}", style.green("created"), written.display());
            Ok(Outcome::Success)
        }

        Command::Run(args) => run_requests(args, &start_dir, style),

        Command::Capture(CaptureCommand::Start { port, session, exclude, only }) => {
            start_capture(&start_dir, port, session, exclude, only, style)
        }

        Command::Capture(CaptureCommand::Ls {
            host,
            url,
            method,
            status,
            status_max,
            session,
            limit,
        }) => {
            let collection = open_collection(&start_dir)?;
            let store = CaptureStore::new(collection.captures_path());
            let filter = CaptureFilter {
                host,
                url,
                method,
                status_min: status,
                status_max,
                session,
            };
            let captures = store.list(&filter, limit)?;
            if captures.is_empty() {
                println!(
                    "{}",
                    style.dim("no captures — run `gabriel capture start` and browse")
                );
            }
            for capture in &captures {
                println!(
                    "{}  {}  {:<6} {}  {}",
                    style.dim(support::display_id(&capture.id)),
                    capture
                        .status()
                        .map(|s| style.status(s))
                        .unwrap_or_else(|| style.dim("---")),
                    style.safe(&capture.request.method),
                    style.safe(&output::truncate(&capture.request.url, 80)),
                    style.dim(&output::format_duration(capture.duration_ms)),
                );
            }
            Ok(Outcome::Success)
        }

        Command::Capture(CaptureCommand::Show { id }) => {
            let collection = open_collection(&start_dir)?;
            let store = CaptureStore::new(collection.captures_path());
            let capture = store
                .get(&id)?
                .with_context(|| format!("no capture matches `{id}`"))?;
            support::print_capture(&capture, style);
            Ok(Outcome::Success)
        }

        Command::Capture(CaptureCommand::Clear) => {
            let collection = open_collection(&start_dir)?;
            let store = CaptureStore::new(collection.captures_path());
            let count = store.count()?;
            store.clear()?;
            println!("{} {count} captures", style.green("cleared"));
            Ok(Outcome::Success)
        }

        Command::Promote(args) => promote(args, &start_dir, style),

        Command::Diff { before, after } => {
            let collection = open_collection(&start_dir)?;
            let store = CaptureStore::new(collection.captures_path());
            let before = store
                .get(&before)?
                .with_context(|| format!("no capture matches `{before}`"))?;
            let after = store
                .get(&after)?
                .with_context(|| format!("no capture matches `{after}`"))?;

            println!(
                "{} {}\n{} {}\n",
                style.dim("before"),
                style.safe(&output::truncate(&before.request.url, 90)),
                style.dim("after "),
                style.safe(&output::truncate(&after.request.url, 90))
            );
            let diff = gabriel_core::diff::diff_responses(
                &support::capture_to_response(&before),
                &support::capture_to_response(&after),
            );
            output::print_diff(&diff, style);
            Ok(Outcome::Success)
        }

        Command::Vault(command) => vault_command(command, &start_dir, style),

        Command::Session(SessionCommand::Ls) => {
            let collection = open_collection(&start_dir)?;
            let sessions = SessionStore::load(collection.sessions_path())?;
            if sessions.names().is_empty() {
                println!("{}", style.dim("no sessions yet — capture some traffic first"));
            }
            for name in sessions.names() {
                println!("{:<20} {} cookies", style.bold(name), sessions.cookie_count(name));
            }
            Ok(Outcome::Success)
        }

        Command::Session(SessionCommand::Clear { name }) => {
            let collection = open_collection(&start_dir)?;
            let mut sessions = SessionStore::load(collection.sessions_path())?;
            let removed = sessions.clear(&name);
            sessions.save()?;
            println!("{} {removed} cookies from `{name}`", style.green("cleared"));
            Ok(Outcome::Success)
        }

        Command::Ws {
            target,
            send,
            messages,
            timeout,
            close_after_send,
            subprotocols,
            env,
            vars,
            session,
        } => ws_command(
            WsArgs {
                target,
                send,
                messages,
                timeout,
                close_after_send,
                subprotocols,
                env,
                vars,
                session,
            },
            &start_dir,
            style,
        ),

        Command::Har(command) => har_command(command, &start_dir, style),

        Command::Jwt { token, capture, json } => jwt_command(token, capture, json, &start_dir, style),

        Command::Curl { request, env, vars, session, show_secrets, one_line } => curl_command(
            CurlArgs { request, env, vars, session, show_secrets, one_line },
            &start_dir,
            style,
        ),

        Command::Env => {
            let collection = open_collection(&start_dir)?;
            let names = collection.environment_names();
            if names.is_empty() {
                println!("{}", style.dim("no environments"));
            }
            for name in names {
                let env = collection.environment(&name)?;
                let vars = env.variables();
                println!(
                    "{:<16} {} variables{}",
                    style.bold(&name),
                    vars.len(),
                    if env.secrets.is_empty() {
                        String::new()
                    } else {
                        format!(" ({} from the vault)", env.secrets.len())
                    }
                );
            }
            Ok(Outcome::Success)
        }

        Command::Ca { pem } => {
            let collection = open_collection(&start_dir)?;
            let ca =
                gabriel_proxy::ca::CertificateAuthority::load_or_create(collection.runtime_dir())?;
            if pem {
                print!("{}", ca.cert_pem());
            } else {
                support::print_ca_instructions(ca.cert_path(), style);
            }
            Ok(Outcome::Success)
        }
    }
}

struct CurlArgs {
    request: String,
    env: Option<String>,
    vars: Vec<String>,
    session: Option<String>,
    show_secrets: bool,
    one_line: bool,
}

struct WsArgs {
    target: String,
    send: Vec<String>,
    messages: usize,
    timeout: u64,
    close_after_send: bool,
    subprotocols: Vec<String>,
    env: Option<String>,
    vars: Vec<String>,
    session: Option<String>,
}

fn ws_command(args: WsArgs, start_dir: &Path, style: &Style) -> Result<Outcome> {
    use gabriel_engine::websocket::{self, Direction, WebSocketPlan};

    let collection = open_collection(start_dir)?;

    // A URL is a request too; nobody should have to create a file to poke a
    // socket once.
    let spec = if args.target.contains("://") {
        gabriel_core::RequestSpec::new("GET", &args.target)
    } else {
        collection.apply_defaults(&collection.find(&args.target)?.spec)
    };

    let env_name = match (&args.env, collection.environment_names().as_slice()) {
        (Some(name), _) => Some(name.clone()),
        (None, [only]) => Some(only.clone()),
        (None, _) => None,
    };
    let environment = env_name.as_deref().map(|n| collection.environment(n)).transpose()?;

    let secrets = LazySecrets::new(collection.vault_path(), KeySource::from_environment());
    let mut resolver = Resolver::new()
        .with_secrets(&secrets)
        .with_vars(collection.variables_for(environment.as_ref()));
    for assignment in &args.vars {
        let (key, value) = support::parse_assignment(assignment)?;
        resolver.set(key, value);
    }

    let mut sessions = SessionStore::load(collection.sessions_path())?;
    let session = args
        .session
        .clone()
        .unwrap_or_else(|| gabriel_engine::session::DEFAULT_SESSION.to_string());

    let plan = WebSocketPlan {
        send: args.send.clone(),
        max_messages: args.messages,
        max_duration: std::time::Duration::from_secs(args.timeout),
        close_after_send: args.close_after_send,
        subprotocols: args.subprotocols.clone(),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    let outcome = {
        let mut ctx = RunContext::new(&mut resolver, &mut sessions)
            .with_session(session)
            .with_base_dir(collection.root());
        let redactor = Redactor::new(ctx.resolver.used_secrets());
        runtime
            .block_on(websocket::run(&spec, &mut ctx, &plan, |frame| {
                let arrow = match frame.direction {
                    Direction::Sent => style.cyan("→"),
                    Direction::Received => style.green("←"),
                };
                println!(
                    "{} {} {}",
                    style.dim(&format!("{:>6}", output::format_duration(frame.at_ms))),
                    arrow,
                    style.safe(&redactor.apply(&frame.payload.summary()))
                );
            }))
            .with_context(|| format!("websocket `{}`", args.target))?
    };

    sessions.save()?;

    println!();
    let sent = outcome.frames.len() - outcome.received().count();
    println!(
        "{} {} · {} sent, {} received in {}",
        style.status(outcome.status),
        style.dim(if outcome.status == 101 { "Switching Protocols" } else { "" }),
        sent,
        outcome.received().count(),
        output::format_duration(outcome.duration_ms)
    );
    if let Some(protocol) = &outcome.subprotocol {
        println!("{} {protocol}", style.dim("subprotocol"));
    }
    let reason = match outcome.ended {
        websocket::SocketEnd::ClosedByServer => "the server closed the socket",
        websocket::SocketEnd::ClosedAfterSend => "closed after sending, as asked",
        websocket::SocketEnd::MessageLimitReached => "reached --messages",
        websocket::SocketEnd::TimedOut => "reached --timeout",
    };
    println!("{}", style.dim(reason));

    Ok(Outcome::Success)
}

fn har_command(command: HarCommand, start_dir: &Path, style: &Style) -> Result<Outcome> {
    let collection = open_collection(start_dir)?;
    let store = CaptureStore::new(collection.captures_path());

    match command {
        HarCommand::Export { out, host, url, method, status, session, limit } => {
            let filter = CaptureFilter {
                host,
                url,
                method,
                status_min: status,
                status_max: None,
                session,
            };
            let captures = store.list(&filter, limit)?;
            let har = gabriel_core::har::export(&captures);
            let text = serde_json::to_string_pretty(&har)?;

            match out {
                Some(path) => {
                    std::fs::write(&path, &text)
                        .with_context(|| format!("writing {}", path.display()))?;
                    eprintln!(
                        "{} {} capture(s) to {}",
                        style.green("exported"),
                        captures.len(),
                        path.display()
                    );
                    // The log holds credentials, and so does anything derived
                    // from it — say so once rather than assume it is obvious.
                    eprintln!(
                        "{} the file contains request headers verbatim, including cookies and tokens",
                        style.yellow("warning:")
                    );
                }
                None => println!("{text}"),
            }
            Ok(Outcome::Success)
        }

        HarCommand::Import { file, session } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let har: gabriel_core::har::Har = serde_json::from_str(&text)
                .with_context(|| format!("{} is not a HAR file", file.display()))?;

            // A prefix keeps imported ids from colliding with recorded ones.
            let prefix = format!("i{:x}", gabriel_core::now_ms());
            let (mut captures, skipped) = gabriel_core::har::import(&har, &prefix);

            if let Some(session) = &session {
                for capture in &mut captures {
                    capture.session = Some(session.clone());
                }
            }

            for capture in &captures {
                store.append(capture)?;
            }

            println!(
                "{} {} capture(s) from {} ({})",
                style.green("imported"),
                captures.len(),
                file.display(),
                har.log.creator.name
            );
            if skipped > 0 {
                eprintln!(
                    "{} skipped {skipped} entr{} with no usable request",
                    style.yellow("note:"),
                    if skipped == 1 { "y" } else { "ies" }
                );
            }
            if !captures.is_empty() {
                println!();
                println!("  {}", style.dim("gabriel capture ls        see what arrived"));
                println!("  {}", style.dim("gabriel promote <id>      turn one into a request"));
            }
            Ok(Outcome::Success)
        }
    }
}

fn jwt_command(
    token: Option<String>,
    capture: Option<String>,
    as_json: bool,
    start_dir: &Path,
    style: &Style,
) -> Result<Outcome> {
    use gabriel_core::jwt::Jwt;
    use std::io::Read as _;

    let raw = match (token.as_deref(), capture.as_deref()) {
        (Some("-"), _) => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).context("reading the token from stdin")?;
            input
        }
        (Some(token), _) => token.to_string(),
        (None, Some(id)) => {
            // Pull it out of recorded traffic, so the developer never has to
            // select and copy a live credential.
            let collection = open_collection(start_dir)?;
            let store = CaptureStore::new(collection.captures_path());
            let capture = store
                .get(id)?
                .with_context(|| format!("no capture matches `{id}`"))?;

            let from_headers = capture
                .request
                .headers
                .iter_pairs()
                .find_map(|(_, value)| gabriel_core::jwt::find_in(value).map(str::to_string));
            let from_body = capture
                .response
                .as_ref()
                .and_then(|r| r.body.as_ref())
                .and_then(|b| b.as_text())
                .and_then(|text| gabriel_core::jwt::find_in(text).map(str::to_string));

            from_headers
                .or(from_body)
                .with_context(|| format!("no JWT found in capture `{id}`"))?
        }
        (None, None) => bail!("pass a token, `-` to read stdin, or --capture <id>"),
    };

    let jwt = Jwt::decode(&raw)?;

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "header": jwt.header,
                "payload": jwt.payload,
            }))?
        );
        return Ok(Outcome::Success);
    }

    let now = gabriel_core::now_ms();
    println!(
        "{} {}{}",
        style.bold("algorithm"),
        jwt.algorithm().unwrap_or("<none declared>"),
        jwt.key_id().map(|k| format!("  kid {k}")).unwrap_or_default()
    );

    for (name, value) in jwt.notable_claims() {
        println!("{:<10} {}", style.dim(name), style.safe(&value));
    }

    for (label, claim) in [("issued", "iat"), ("not before", "nbf"), ("expires", "exp")] {
        if let Some(ms) = jwt.time_claim_ms(claim) {
            let relative = match jwt.expires_in_ms(now) {
                Some(remaining) if claim == "exp" && remaining > 0 => {
                    format!("  in {}", output::format_duration(remaining as u64))
                }
                Some(remaining) if claim == "exp" => {
                    format!("  {} ago", output::format_duration((-remaining) as u64))
                }
                _ => String::new(),
            };
            println!(
                "{:<10} {}{}",
                style.dim(label),
                gabriel_core::format_iso8601(ms),
                style.dim(&relative)
            );
        }
    }

    if !jwt.warnings.is_empty() {
        println!();
        for warning in &jwt.warnings {
            let marker = if warning.is_serious() { style.red("!") } else { style.yellow("·") };
            println!("{marker} {}", warning.message());
        }
    }

    println!();
    println!(
        "{}",
        style.dim("signature not verified — that needs the issuer's key, which Gabriel does not have")
    );

    // A serious finding is worth a non-zero exit so a script can gate on it.
    Ok(if jwt.warnings.iter().any(|w| w.is_serious()) {
        Outcome::AssertionsFailed
    } else {
        Outcome::Success
    })
}

fn curl_command(args: CurlArgs, start_dir: &Path, style: &Style) -> Result<Outcome> {
    let collection = open_collection(start_dir)?;
    let entry = collection.find(&args.request)?.clone();

    let env_name = match (&args.env, collection.environment_names().as_slice()) {
        (Some(name), _) => Some(name.clone()),
        (None, [only]) => Some(only.clone()),
        (None, _) => None,
    };
    let environment = env_name.as_deref().map(|n| collection.environment(n)).transpose()?;

    let secrets = LazySecrets::new(collection.vault_path(), KeySource::from_environment());
    let mut resolver = Resolver::new()
        .with_secrets(&secrets)
        .with_vars(collection.variables_for(environment.as_ref()));
    for assignment in &args.vars {
        let (key, value) = support::parse_assignment(assignment)?;
        resolver.set(key, value);
    }

    let mut sessions = SessionStore::load(collection.sessions_path())?;
    let session = args
        .session
        .clone()
        .unwrap_or_else(|| gabriel_engine::session::DEFAULT_SESSION.to_string());

    let spec = collection.apply_defaults(&entry.spec);
    let mut executor = Executor::new();
    let (prepared, oauth_pending) = {
        let mut ctx = RunContext::new(&mut resolver, &mut sessions)
            .with_session(session)
            .with_base_dir(collection.root());
        executor.prepare(&spec, &mut ctx)?
    };

    let redactor = if args.show_secrets {
        Redactor::default()
    } else {
        Redactor::new(resolver.used_secrets())
    };

    if oauth_pending {
        eprintln!(
            "{} this request uses OAuth2; the Authorization header is omitted because \
             generating it would require fetching a token",
            style.yellow("note:")
        );
    }
    if !args.show_secrets && !redactor.is_empty() {
        eprintln!(
            "{} credentials are masked — pass --show-secrets for a runnable command",
            style.dim("note:")
        );
    }

    println!("{}", codegen::to_curl(&prepared, &redactor, !args.one_line));
    Ok(Outcome::Success)
}

fn open_collection(dir: &Path) -> Result<Collection> {
    Ok(Collection::discover(dir)?)
}

fn run_requests(args: RunArgs, start_dir: &Path, style: &Style) -> Result<Outcome> {
    let collection = open_collection(start_dir)?;

    // With a single environment, requiring `--env` is bureaucracy.
    let env_name = match (&args.env, collection.environment_names().as_slice()) {
        (Some(name), _) => Some(name.clone()),
        (None, [only]) => Some(only.clone()),
        (None, _) => None,
    };
    let environment = env_name.as_deref().map(|n| collection.environment(n)).transpose()?;

    let targets: Vec<gabriel_collection::RequestEntry> = if args.all {
        collection.requests().to_vec()
    } else {
        let query = args.request.as_deref().unwrap_or_default();
        vec![collection.find(query)?.clone()]
    };

    let secrets = LazySecrets::new(collection.vault_path(), KeySource::from_environment());
    let mut resolver = Resolver::new()
        .with_secrets(&secrets)
        .with_process_env(args.allow_env)
        .with_vars(collection.variables_for(environment.as_ref()));
    for assignment in &args.vars {
        let (key, value) = support::parse_assignment(assignment)?;
        resolver.set(key, value);
    }

    let mut sessions = SessionStore::load(collection.sessions_path())?;
    sessions.set_path(collection.sessions_path());
    let session = args
        .session
        .clone()
        .unwrap_or_else(|| gabriel_engine::session::DEFAULT_SESSION.to_string());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    let mut executor = Executor::new();
    let mut all_passed = true;
    let mut errors = 0usize;
    let wants_report = args.junit.is_some() || args.html.is_some();
    let mut cases: Vec<report::CaseResult> = Vec::new();
    let started_ms = gabriel_core::now_ms();

    // Streaming is a single-request mode: following two event streams at once
    // and interleaving their output would be unreadable.
    if args.stream {
        let entry = &targets[0];
        let spec = collection.apply_defaults(&entry.spec);
        let limits = gabriel_engine::StreamLimits {
            max_events: args.events,
            max_duration: std::time::Duration::from_secs(args.stream_timeout),
        };
        let redactor_secrets = resolver.used_secrets();

        let outcome = {
            let mut ctx = RunContext::new(&mut resolver, &mut sessions)
                .with_session(session.clone())
                .with_base_dir(collection.root());
            let stream_style = style;
            let redactor = if args.show_secrets {
                Redactor::default()
            } else {
                Redactor::new(redactor_secrets)
            };
            let mut index = 0usize;
            runtime
                .block_on(executor.execute_stream(&spec, &mut ctx, &limits, |event| {
                    index += 1;
                    let name = event.name.as_deref().unwrap_or("message");
                    // Pretty-print JSON payloads; most streaming APIs send them.
                    let data = match event.json() {
                        Some(json) => serde_json::to_string(&json).unwrap_or_else(|_| event.data.clone()),
                        None => event.data.clone(),
                    };
                    println!(
                        "{} {} {}",
                        stream_style.dim(&format!("{index:>4}")),
                        stream_style.cyan(name),
                        stream_style.safe(&redactor.apply(&data))
                    );
                }))
                .with_context(|| format!("streaming `{}`", entry.id))?
        };

        println!();
        println!(
            "{} {} {} {} events in {}",
            style.status(outcome.status),
            style.dim(&outcome.status_text),
            style.dim("·"),
            outcome.events.len(),
            output::format_duration(outcome.duration_ms)
        );
        match outcome.ended {
            gabriel_engine::StreamEnd::Closed => {}
            gabriel_engine::StreamEnd::LimitReached => println!(
                "{}",
                style.dim(&format!("stopped at --events {}", args.events))
            ),
            gabriel_engine::StreamEnd::TimedOut => println!(
                "{}",
                style.dim(&format!("stopped after --stream-timeout {}s", args.stream_timeout))
            ),
            gabriel_engine::StreamEnd::NotAStream => {
                let content_type = outcome.headers.get_first("content-type").unwrap_or("none");
                println!(
                    "{} content-type is {content_type}, not text/event-stream — run without --stream",
                    style.yellow("note:")
                );
            }
        }

        sessions.save()?;
        return Ok(Outcome::Success);
    }

    for (index, entry) in targets.iter().enumerate() {
        let spec = collection.apply_defaults(&entry.spec);
        if targets.len() > 1 {
            if index > 0 {
                println!();
            }
            println!("{}", style.bold(&format!("── {} ", entry.id)));
        }

        let attempt = {
            let mut ctx = RunContext::new(&mut resolver, &mut sessions)
                .with_session(session.clone())
                .with_base_dir(collection.root());
            runtime
                .block_on(executor.execute(&spec, &mut ctx))
                .with_context(|| format!("running `{}`", entry.id))
        };

        // Running a whole collection is a reporting job: one request that
        // cannot even be sent must not hide the results of the rest. A single
        // named request still fails outright.
        let outcome = match attempt {
            Ok(outcome) => outcome,
            Err(error) if args.all => {
                eprintln!("{} {error:#}", style.red("error:"));
                if wants_report {
                    cases.push(report::CaseResult {
                        id: entry.id.clone(),
                        method: spec.method.clone(),
                        url: spec.url.clone(),
                        status: None,
                        duration_ms: 0,
                        outcome: report::CaseOutcome::Errored(format!("{error:#}")),
                        assertions: Vec::new(),
                    });
                }
                errors += 1;
                all_passed = false;
                continue;
            }
            Err(error) => return Err(error),
        };

        if wants_report {
            cases.push(report::CaseResult {
                id: entry.id.clone(),
                method: outcome.sent.method.clone(),
                url: outcome.sent.url.clone(),
                status: Some(outcome.response.status),
                duration_ms: outcome.response.timings.total_ms,
                outcome: if outcome.assertions_passed() {
                    report::CaseOutcome::Passed
                } else {
                    report::CaseOutcome::Failed
                },
                assertions: outcome
                    .assertions
                    .iter()
                    .map(|a| report::AssertionLine {
                        description: a.description.clone(),
                        passed: a.passed,
                        actual: a.actual.clone(),
                    })
                    .collect(),
            });
        }

        let redactor = if args.show_secrets {
            Redactor::default()
        } else {
            Redactor::new(resolver.used_secrets())
        };

        if args.quiet {
            // `safe` is a no-op when stdout is a pipe, so `| jq` still gets the
            // exact bytes; at a terminal the escapes are defused.
            println!(
                "{}",
                style.safe(&redactor.apply(&output::render_body(&outcome.response, usize::MAX)))
            );
        } else {
            output::print_run(&outcome, style, &redactor, args.verbose, args.body_limit);
        }
        all_passed &= outcome.assertions_passed();
    }

    sessions.save()?;

    if wants_report {
        let run = report::RunReport {
            collection: collection
                .manifest()
                .name
                .clone()
                .unwrap_or_else(|| "collection".to_string()),
            environment: env_name.clone(),
            started_ms,
            cases,
        };
        // A report is an artifact that outlives the run; the same redaction that
        // protects the terminal protects it.
        let redactor = if args.show_secrets {
            Redactor::default()
        } else {
            Redactor::new(resolver.used_secrets())
        };

        if let Some(path) = &args.junit {
            std::fs::write(path, report::to_junit(&run, &redactor))
                .with_context(|| format!("writing {}", path.display()))?;
            eprintln!("{} {}", style.dim("junit report"), path.display());
        }
        if let Some(path) = &args.html {
            std::fs::write(path, report::to_html(&run, &redactor))
                .with_context(|| format!("writing {}", path.display()))?;
            eprintln!("{} {}", style.dim("html report"), path.display());
        }
    }

    if errors > 0 {
        // Report every failure that was skipped over, then fail the run.
        bail!(
            "{errors} of {} request(s) could not be sent",
            targets.len()
        );
    }
    Ok(if all_passed { Outcome::Success } else { Outcome::AssertionsFailed })
}

fn start_capture(
    start_dir: &Path,
    port: u16,
    session: String,
    exclude: Vec<String>,
    only: Vec<String>,
    style: &Style,
) -> Result<Outcome> {
    let collection = open_collection(start_dir)?;
    let store = CaptureStore::new(collection.captures_path());
    let mut sessions = SessionStore::load(collection.sessions_path())?;
    sessions.set_path(collection.sessions_path());

    let config = ProxyConfig {
        addr: ([127, 0, 0, 1], port).into(),
        session: session.clone(),
        exclude: exclude.clone(),
        only: only.clone(),
        ..Default::default()
    };
    let proxy = Proxy::new(config, collection.runtime_dir(), store, sessions)?;
    let ca_path = proxy.ca_cert_path().to_path_buf();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    runtime.block_on(async move {
        let running = proxy.start().await?;
        println!("{} http://{}", style.green("proxy listening on"), running.addr);
        if let Some(warning) = gabriel_core::permission_warning() {
            eprintln!("{} {warning}", style.yellow("warning:"));
        }
        println!("  {} {}", style.dim("session:"), session);
        if !only.is_empty() {
            println!("  {} {}", style.dim("intercepting only:"), only.join(", "));
        }
        if !exclude.is_empty() {
            println!("  {} {}", style.dim("tunnelling untouched:"), exclude.join(", "));
        }
        println!("  {} {}", style.dim("CA certificate:"), ca_path.display());
        println!();
        println!("{}", style.dim("HTTPS needs the CA trusted once — see `gabriel ca`."));
        println!("{}", style.dim("Ctrl-C to stop."));

        tokio::signal::ctrl_c().await.ok();
        println!();
        println!("{}", style.dim("stopped"));
        running.shutdown().await;
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(Outcome::Success)
}

fn promote(args: PromoteArgs, start_dir: &Path, style: &Style) -> Result<Outcome> {
    let mut collection = open_collection(start_dir)?;
    let store = CaptureStore::new(collection.captures_path());
    let capture = store
        .get(&args.id)?
        .with_context(|| format!("no capture matches `{}`", args.id))?;

    let mut options = PromoteOptions {
        inline_cookies: args.inline_cookies,
        inline_token: args.inline_token,
        base_url_var: Some("base_url".to_string()),
    };
    let mut promotion = capture.promote(&options);

    // Parameterising the origin as `{{base_url}}` is only helpful if that
    // variable resolves back to the host the request was captured from. If an
    // environment already points `base_url` somewhere else, substituting it
    // would save a request that silently targets the wrong server — so keep the
    // literal origin and say so.
    let captured_origin = promotion
        .vars
        .iter()
        .find(|(name, _)| name == "base_url")
        .map(|(_, value)| value.clone());
    if args.env.is_none()
        && let Some(origin) = &captured_origin
    {
        let conflicts: Vec<(String, String)> = collection
            .environment_names()
            .into_iter()
            .filter_map(|name| {
                let existing = collection.environment(&name).ok()?.variables().get("base_url")?.clone();
                (existing != *origin).then_some((name, existing))
            })
            .collect();

        if !conflicts.is_empty() {
            options.base_url_var = None;
            promotion = capture.promote(&options);
            for (env_name, existing) in &conflicts {
                eprintln!(
                    "{} `{env_name}` already sets base_url to {existing}, not {origin} — \
                     keeping the literal URL so the replay hits the host it was captured from",
                    style.yellow("note:")
                );
            }
        }
    }

    let path = match args.to {
        Some(path) => path,
        None => collection.unique_request_path(promotion.spec.display_name()),
    };

    // An explicit --to can name a request that already exists, and that file may
    // have been edited by hand. Overwriting it silently loses work.
    if !args.force
        && let Ok(existing) = collection.find(&path)
        && existing.id == path.trim_matches('/').trim_end_matches(".toml")
    {
        bail!(
            "{} already exists — pass --force to replace it, or --to <other-path>",
            existing.path.display()
        );
    }

    let written = collection.save_request(&path, &promotion.spec)?;
    println!("{} {}", style.green("saved"), written.display());

    if !promotion.secrets.is_empty() {
        let mut vault = Vault::open(collection.vault_path(), &KeySource::from_environment())
            .context("opening the vault to store the captured credentials")?;
        for (name, value) in &promotion.secrets {
            vault.set(name, value);
            println!("{} {}", style.green("vaulted"), name);
        }
        vault.save()?;
    }

    if let Some(env_name) = &args.env {
        for (name, value) in &promotion.vars {
            if support::set_environment_var(&collection, env_name, name, value)? {
                println!("{} {name} = {value} in {env_name}", style.green("set"));
            }
        }
    } else {
        for (name, value) in &promotion.vars {
            println!(
                "{} {name} is {value} — bind it with {}",
                style.dim("note:"),
                style.dim(&format!(
                    "gabriel promote {} --env <name>",
                    support::display_id(&capture.id)
                ))
            );
        }
    }

    if matches!(promotion.spec.auth, Some(gabriel_core::model::Auth::Session { .. })) {
        println!(
            "{} replays will use the `{}` session's cookies",
            style.dim("note:"),
            promotion.session.as_deref().unwrap_or("default")
        );
    }

    println!();
    println!("  {}", style.dim(&format!("gabriel run {path}")));
    Ok(Outcome::Success)
}

fn vault_command(command: VaultCommand, start_dir: &Path, style: &Style) -> Result<Outcome> {
    let collection = open_collection(start_dir)?;
    let key_source = KeySource::from_environment();

    match command {
        VaultCommand::Set { name, value } => {
            let mut vault = Vault::open(collection.vault_path(), &key_source)?;
            vault.set(&name, value);
            vault.save()?;
            println!("{} {name}", style.green("stored"));
        }
        VaultCommand::Ls => {
            if !collection.vault_path().exists() {
                println!(
                    "{}",
                    style.dim("no vault yet — `gabriel vault set <name> <value>`")
                );
                return Ok(Outcome::Success);
            }
            let vault = Vault::open(collection.vault_path(), &key_source)?;
            if vault.is_empty() {
                println!("{}", style.dim("vault is empty"));
            }
            for name in vault.names() {
                println!("{name}");
            }
        }
        VaultCommand::Rm { name } => {
            let mut vault = Vault::open(collection.vault_path(), &key_source)?;
            if !vault.remove(&name) {
                bail!("no secret named `{name}`");
            }
            vault.save()?;
            println!("{} {name}", style.green("removed"));
        }
    }
    Ok(Outcome::Success)
}
