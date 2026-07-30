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

mod output;
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
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        method: Option<String>,
        /// Only show responses at or above this status, e.g. `--status 400`.
        #[arg(long)]
        status: Option<u16>,
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

        Command::Capture(CaptureCommand::Ls { host, method, status, limit }) => {
            let collection = open_collection(&start_dir)?;
            let store = CaptureStore::new(collection.captures_path());
            let filter = CaptureFilter { host, method, status_min: status, ..Default::default() };
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
                    capture.request.method,
                    output::truncate(&capture.request.url, 80),
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
                output::truncate(&before.request.url, 90),
                style.dim("after "),
                output::truncate(&after.request.url, 90)
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

    for (index, entry) in targets.iter().enumerate() {
        let spec = collection.apply_defaults(&entry.spec);
        if targets.len() > 1 {
            if index > 0 {
                println!();
            }
            println!("{}", style.bold(&format!("── {} ", entry.id)));
        }

        let outcome = {
            let mut ctx = RunContext::new(&mut resolver, &mut sessions)
                .with_session(session.clone())
                .with_base_dir(collection.root());
            runtime
                .block_on(executor.execute(&spec, &mut ctx))
                .with_context(|| format!("running `{}`", entry.id))?
        };

        let redactor = if args.show_secrets {
            Redactor::default()
        } else {
            Redactor::new(resolver.used_secrets())
        };

        if args.quiet {
            println!(
                "{}",
                redactor.apply(&output::render_body(&outcome.response, usize::MAX))
            );
        } else {
            output::print_run(&outcome, style, &redactor, args.verbose, args.body_limit);
        }
        all_passed &= outcome.assertions_passed();
    }

    sessions.save()?;

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

    let options = PromoteOptions {
        inline_cookies: args.inline_cookies,
        inline_token: args.inline_token,
        base_url_var: Some("base_url".to_string()),
    };
    let promotion = capture.promote(&options);

    let path = match args.to {
        Some(path) => path,
        None => collection.unique_request_path(promotion.spec.display_name()),
    };
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
