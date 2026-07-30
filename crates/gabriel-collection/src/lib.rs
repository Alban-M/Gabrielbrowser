//! Collections on disk.
//!
//! A collection is a folder, a request is a file, and an environment is a file.
//! There is no database, no workspace id, and no account — `git log` is the
//! history and a pull request is the review. The layout:
//!
//! ```text
//! gabriel/
//!   collection.toml           name, shared defaults, shared variables
//!   environments/
//!     dev.toml                variables and secret bindings for one target
//!   requests/
//!     users/get-user.toml     one request
//!   .runtime/                 vault, captures, sessions — gitignored
//! ```

use gabriel_core::model::{Auth, FieldMap, RequestSpec, Settings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const COLLECTION_DIR: &str = "gabriel";
pub const MANIFEST_FILE: &str = "collection.toml";
pub const REQUESTS_DIR: &str = "requests";
pub const ENVIRONMENTS_DIR: &str = "environments";
/// Everything machine-local and/or sensitive. Gitignored by `init`.
pub const RUNTIME_DIR: &str = ".runtime";

#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    #[error("no Gabriel collection found in {0} or any parent — run `gabriel init` to create one")]
    NotFound(PathBuf),

    #[error("{path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("no request matches `{0}`")]
    NoSuchRequest(String),

    #[error("`{query}` matches {} requests: {}", .matches.len(), .matches.join(", "))]
    AmbiguousRequest { query: String, matches: Vec<String> },

    #[error("no environment named `{name}` (have: {})", if .available.is_empty() { "none".to_string() } else { .available.join(", ") })]
    NoSuchEnvironment {
        name: String,
        available: Vec<String>,
    },

    #[error("a collection already exists at {0}")]
    AlreadyExists(PathBuf),

    #[error(
        "`{0}` would write outside the collection — request paths are relative to `requests/` and may not contain `..`"
    )]
    UnsafePath(String),
}

type Result<T> = std::result::Result<T, CollectionError>;

/// `collection.toml` — shared settings every request in the tree inherits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Variables available to every request, below environment variables in
    /// precedence.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Defaults::is_empty")]
    pub defaults: Defaults,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default, skip_serializing_if = "FieldMap::is_empty")]
    pub headers: FieldMap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Settings>,
}

impl Defaults {
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty() && self.auth.is_none() && self.settings.is_none()
    }
}

/// One environment file: where the same requests point at a different target.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Environment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vars: BTreeMap<String, String>,
    /// Variable name → vault key. Sugar for `var = "{{secret:key}}"`, but it
    /// also documents, in the committed file, exactly which values are
    /// sensitive — without revealing any of them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, String>,
}

impl Environment {
    /// Flatten to the variable map the resolver consumes.
    pub fn variables(&self) -> BTreeMap<String, String> {
        let mut vars = self.vars.clone();
        for (name, vault_key) in &self.secrets {
            vars.insert(name.clone(), format!("{{{{secret:{vault_key}}}}}"));
        }
        vars
    }
}

#[derive(Debug, Clone)]
pub struct RequestEntry {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// Slash-separated path under `requests/`, without the extension.
    /// This is the name a developer types.
    pub id: String,
    pub spec: RequestSpec,
}

/// A request file that could not be read. Held rather than raised, so one bad
/// file does not take the whole collection down with it.
#[derive(Debug, Clone)]
pub struct LoadProblem {
    pub path: PathBuf,
    /// Id the request would have had, so a `run` naming it can explain itself.
    pub id: String,
    pub message: String,
}

pub struct Collection {
    root: PathBuf,
    manifest: Manifest,
    requests: Vec<RequestEntry>,
    problems: Vec<LoadProblem>,
}

impl Collection {
    /// Walk up from `start` looking for a `gabriel/` directory, the way `git`
    /// finds `.git`. Running `gabriel run login` from anywhere in the repo works.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        let start = start
            .as_ref()
            .canonicalize()
            .unwrap_or_else(|_| start.as_ref().to_path_buf());
        for dir in start.ancestors() {
            let candidate = dir.join(COLLECTION_DIR);
            if candidate.join(MANIFEST_FILE).is_file() {
                return Self::load(candidate);
            }
            // Also accept being run from inside the collection directory.
            if dir.join(MANIFEST_FILE).is_file()
                && dir.file_name().is_some_and(|n| n == COLLECTION_DIR)
            {
                return Self::load(dir);
            }
        }
        Err(CollectionError::NotFound(start))
    }

    pub fn load(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let manifest_path = root.join(MANIFEST_FILE);
        let manifest: Manifest = read_toml(&manifest_path)?;

        let mut requests = Vec::new();
        let mut problems: Vec<LoadProblem> = Vec::new();
        let requests_root = root.join(REQUESTS_DIR);
        if requests_root.is_dir() {
            for entry in walkdir::WalkDir::new(&requests_root)
                .sort_by_file_name()
                .into_iter()
                .filter_map(std::result::Result::ok)
            {
                let path = entry.path();
                if !entry.file_type().is_file()
                    || path.extension().and_then(|e| e.to_str()) != Some("toml")
                {
                    continue;
                }
                let id = request_id(&requests_root, path);
                // A collection is edited by hand and shared through Git, so a
                // teammate's malformed file must not stop everyone else from
                // running anything. Same reasoning as the capture log, which
                // skips a corrupt line rather than failing the read.
                let mut spec: RequestSpec = match read_toml(path) {
                    Ok(spec) => spec,
                    Err(error) => {
                        // Store the cause alone: callers re-attach the path, and
                        // printing it twice reads like a bug.
                        let message = match &error {
                            CollectionError::Parse { message, .. } => message.clone(),
                            other => other.to_string(),
                        };
                        problems.push(LoadProblem {
                            path: path.to_path_buf(),
                            id,
                            message,
                        });
                        continue;
                    }
                };
                if spec.name.is_none() {
                    spec.name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string);
                }
                requests.push(RequestEntry {
                    path: path.to_path_buf(),
                    id,
                    spec,
                });
            }
        }

        Ok(Collection {
            root,
            manifest,
            requests,
            problems,
        })
    }

    /// Create a new collection, with a starter request and environment so the
    /// first `gabriel run` works without any typing.
    pub fn init(dir: impl AsRef<Path>, name: &str) -> Result<Self> {
        let root = dir.as_ref().join(COLLECTION_DIR);
        if root.join(MANIFEST_FILE).exists() {
            return Err(CollectionError::AlreadyExists(root));
        }

        create_dir(&root.join(REQUESTS_DIR))?;
        create_dir(&root.join(ENVIRONMENTS_DIR))?;
        create_dir(&root.join(RUNTIME_DIR))?;

        let manifest = Manifest {
            name: Some(name.to_string()),
            ..Default::default()
        };
        write_toml(&root.join(MANIFEST_FILE), &manifest)?;

        let env = Environment {
            name: Some("dev".to_string()),
            vars: [("base_url".to_string(), "https://httpbin.org".to_string())].into(),
            ..Default::default()
        };
        write_toml(&root.join(ENVIRONMENTS_DIR).join("dev.toml"), &env)?;

        let mut example = RequestSpec::new("GET", "{{base_url}}/get");
        example.name = Some("Example request".to_string());
        example.headers.set("Accept", "application/json");
        write_toml(&root.join(REQUESTS_DIR).join("example.toml"), &example)?;

        // The runtime directory holds the vault, cookie jars and captured
        // traffic. None of it belongs in a commit.
        write_file(
            &root.join(RUNTIME_DIR).join(".gitignore"),
            "# Machine-local: vault, sessions, captured traffic.\n*\n",
        )?;

        Self::load(root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn requests(&self) -> &[RequestEntry] {
        &self.requests
    }

    /// Request files that failed to parse. Callers should report these rather
    /// than pretend the collection is complete.
    pub fn problems(&self) -> &[LoadProblem] {
        &self.problems
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.root.join(RUNTIME_DIR)
    }

    pub fn vault_path(&self) -> PathBuf {
        self.runtime_dir().join("vault.json")
    }

    pub fn captures_path(&self) -> PathBuf {
        self.runtime_dir().join("captures.ndjson")
    }

    pub fn sessions_path(&self) -> PathBuf {
        self.runtime_dir().join("sessions.json")
    }

    /// Find one request by id, by suffix, or by name — whichever the developer
    /// typed. Ambiguity is an error rather than a guess.
    pub fn find(&self, query: &str) -> Result<&RequestEntry> {
        let query_norm = query.trim_end_matches(".toml").trim_matches('/');

        if let Some(exact) = self.requests.iter().find(|r| r.id == query_norm) {
            return Ok(exact);
        }

        let matches: Vec<&RequestEntry> = self
            .requests
            .iter()
            .filter(|r| {
                r.id.ends_with(&format!("/{query_norm}"))
                    || r.spec.name.as_deref() == Some(query)
                    || r.id.to_lowercase().contains(&query_norm.to_lowercase())
            })
            .collect();

        match matches.len() {
            0 => {
                // If the thing they asked for is the file that failed to parse,
                // say so — "no request matches" would be a lie.
                if let Some(problem) = self.problems.iter().find(|p| {
                    p.id == query_norm
                        || p.id.ends_with(&format!("/{query_norm}"))
                        || p.id.to_lowercase().contains(&query_norm.to_lowercase())
                }) {
                    return Err(CollectionError::Parse {
                        path: problem.path.clone(),
                        message: problem.message.clone(),
                    });
                }
                Err(CollectionError::NoSuchRequest(query.to_string()))
            }
            1 => Ok(matches[0]),
            _ => Err(CollectionError::AmbiguousRequest {
                query: query.to_string(),
                matches: matches.iter().map(|r| r.id.clone()).collect(),
            }),
        }
    }

    pub fn environment_names(&self) -> Vec<String> {
        let dir = self.root.join(ENVIRONMENTS_DIR);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
            .filter_map(|e| e.path().file_stem()?.to_str().map(str::to_string))
            .collect();
        names.sort();
        names
    }

    pub fn environment(&self, name: &str) -> Result<Environment> {
        let path = self
            .root
            .join(ENVIRONMENTS_DIR)
            .join(format!("{name}.toml"));
        if !path.is_file() {
            return Err(CollectionError::NoSuchEnvironment {
                name: name.to_string(),
                available: self.environment_names(),
            });
        }
        let mut env: Environment = read_toml(&path)?;
        env.name.get_or_insert_with(|| name.to_string());
        Ok(env)
    }

    /// Manifest variables merged under an environment's, which win.
    pub fn variables_for(&self, environment: Option<&Environment>) -> BTreeMap<String, String> {
        let mut vars = self.manifest.vars.clone();
        if let Some(env) = environment {
            vars.extend(env.variables());
        }
        vars
    }

    /// Apply collection-wide defaults to a request: shared headers fill gaps
    /// (a request's own header always wins) and `auth = "inherit"` resolves.
    pub fn apply_defaults(&self, spec: &RequestSpec) -> RequestSpec {
        let mut spec = spec.clone();
        let defaults = &self.manifest.defaults;

        for (key, value) in defaults.headers.iter_pairs() {
            if !spec.headers.contains_key(key) {
                spec.headers.insert(key, value);
            }
        }

        let inherits = matches!(spec.auth, None | Some(Auth::Inherit));
        if inherits && let Some(auth) = defaults.auth.clone() {
            spec.auth = Some(auth);
        }

        if spec.settings.is_default()
            && let Some(settings) = defaults.settings.clone()
        {
            spec.settings = settings;
        }

        spec
    }

    /// Write a request into the collection at `rel` (e.g. `users/get-user`),
    /// creating parent directories. Returns the path written.
    pub fn save_request(&mut self, rel: &str, spec: &RequestSpec) -> Result<PathBuf> {
        let rel = sanitize_request_path(rel)?;
        let rel = rel.as_str();
        let path = self.root.join(REQUESTS_DIR).join(format!("{rel}.toml"));
        if let Some(parent) = path.parent() {
            create_dir(parent)?;
        }
        write_toml(&path, spec)?;

        let id = rel.to_string();
        let entry = RequestEntry {
            path: path.clone(),
            id: id.clone(),
            spec: spec.clone(),
        };
        match self.requests.iter_mut().find(|r| r.id == id) {
            Some(existing) => *existing = entry,
            None => {
                self.requests.push(entry);
                self.requests.sort_by(|a, b| a.id.cmp(&b.id));
            }
        }
        Ok(path)
    }

    /// A path that doesn't collide with an existing request: `get-user`,
    /// `get-user-2`, `get-user-3`…
    pub fn unique_request_path(&self, preferred: &str) -> String {
        let base = slugify(preferred);
        if !self.requests.iter().any(|r| r.id == base) {
            return base;
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|candidate| !self.requests.iter().any(|r| &r.id == candidate))
            .expect("infinite sequence yields a free name")
    }
}

/// Normalise a request path and refuse anything that escapes `requests/`.
///
/// `save_request` joins this onto the collection root, so without this a
/// `--to ../../../elsewhere` would write wherever it liked. Paths are data, and
/// data that becomes a filesystem path gets validated.
fn sanitize_request_path(rel: &str) -> Result<String> {
    // Checked before trimming: silently turning `/etc/passwd` into
    // `requests/etc/passwd` is contained but surprising, and surprise is how
    // people lose files.
    if Path::new(rel.trim()).is_absolute() {
        return Err(CollectionError::UnsafePath(rel.to_string()));
    }

    let trimmed = rel.trim().trim_matches('/').trim_end_matches(".toml");
    if trimmed.is_empty() {
        return Err(CollectionError::UnsafePath(rel.to_string()));
    }
    let candidate = Path::new(trimmed);

    let mut parts = Vec::new();
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(part) => {
                let part = part.to_string_lossy();
                // A backslash is a separator on Windows and an ordinary
                // character elsewhere; treating it as a name would let one
                // platform's collection escape on the other.
                if part.contains('\\') {
                    return Err(CollectionError::UnsafePath(rel.to_string()));
                }
                parts.push(part.into_owned());
            }
            // `.` is harmless but pointless; everything else escapes or is
            // platform-specific (`..`, `/`, `C:`).
            std::path::Component::CurDir => {}
            _ => return Err(CollectionError::UnsafePath(rel.to_string())),
        }
    }

    if parts.is_empty() {
        return Err(CollectionError::UnsafePath(rel.to_string()));
    }
    Ok(parts.join("/"))
}

fn request_id(requests_root: &Path, path: &Path) -> String {
    path.strip_prefix(requests_root)
        .unwrap_or(path)
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Turn "POST /v1/users" into "post-v1-users".
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = true;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "request".to_string()
    } else {
        trimmed
    }
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path).map_err(|source| CollectionError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|e| CollectionError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

pub fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value).map_err(|e| CollectionError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    write_file(path, &text)
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }
    std::fs::write(path, contents).map_err(|source| CollectionError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|source| CollectionError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests run in parallel, so each one needs a directory of its own.
    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gabriel-collection-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn init_creates_a_runnable_collection() {
        let dir = temp_dir();
        let collection = Collection::init(&dir, "demo").unwrap();

        assert_eq!(collection.manifest().name.as_deref(), Some("demo"));
        assert_eq!(collection.requests().len(), 1);
        assert_eq!(collection.environment_names(), vec!["dev"]);
    }

    #[test]
    fn init_refuses_to_overwrite() {
        let dir = temp_dir();
        Collection::init(&dir, "demo").unwrap();
        assert!(matches!(
            Collection::init(&dir, "demo"),
            Err(CollectionError::AlreadyExists(_))
        ));
    }

    #[test]
    fn the_runtime_directory_is_gitignored() {
        let dir = temp_dir();
        let collection = Collection::init(&dir, "demo").unwrap();
        let ignore = std::fs::read_to_string(collection.runtime_dir().join(".gitignore")).unwrap();
        assert!(
            ignore.contains('*'),
            "vault and captures would be committed"
        );
        // Everything holding credentials must live under it.
        assert!(
            collection
                .vault_path()
                .starts_with(collection.runtime_dir())
        );
        assert!(
            collection
                .sessions_path()
                .starts_with(collection.runtime_dir())
        );
        assert!(
            collection
                .captures_path()
                .starts_with(collection.runtime_dir())
        );
    }

    #[test]
    fn discovery_walks_up_from_a_subdirectory() {
        let dir = temp_dir();
        Collection::init(&dir, "demo").unwrap();
        let nested = dir.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        let found = Collection::discover(&nested).unwrap();
        assert_eq!(found.manifest().name.as_deref(), Some("demo"));
    }

    #[test]
    fn discovery_fails_with_a_useful_error() {
        let dir = temp_dir();
        assert!(matches!(
            Collection::discover(&dir),
            Err(CollectionError::NotFound(_))
        ));
    }

    #[test]
    fn nested_requests_get_slash_separated_ids() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        collection
            .save_request(
                "users/get-user",
                &RequestSpec::new("GET", "{{base_url}}/users/1"),
            )
            .unwrap();

        let reloaded = Collection::load(collection.root()).unwrap();
        let ids: Vec<&str> = reloaded.requests().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["example", "users/get-user"]);
    }

    #[test]
    fn find_accepts_id_suffix_or_name() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        let mut spec = RequestSpec::new("GET", "{{base_url}}/users/1");
        spec.name = Some("Fetch one user".into());
        collection.save_request("users/get-user", &spec).unwrap();

        assert_eq!(
            collection.find("users/get-user").unwrap().id,
            "users/get-user"
        );
        assert_eq!(collection.find("get-user").unwrap().id, "users/get-user");
        assert_eq!(
            collection.find("Fetch one user").unwrap().id,
            "users/get-user"
        );
    }

    #[test]
    fn ambiguous_queries_list_the_candidates_instead_of_guessing() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        let spec = RequestSpec::new("GET", "https://x.test");
        collection.save_request("a/list", &spec).unwrap();
        collection.save_request("b/list", &spec).unwrap();

        let message = collection.find("list").unwrap_err().to_string();
        assert!(
            message.contains("a/list") && message.contains("b/list"),
            "{message}"
        );
    }

    #[test]
    fn environment_secrets_become_vault_references() {
        let dir = temp_dir();
        let collection = Collection::init(&dir, "demo").unwrap();
        let env = Environment {
            name: Some("prod".into()),
            description: None,
            vars: [("base_url".to_string(), "https://api.test".to_string())].into(),
            secrets: [("api_token".to_string(), "prod_api_token".to_string())].into(),
        };
        write_toml(
            &collection.root().join(ENVIRONMENTS_DIR).join("prod.toml"),
            &env,
        )
        .unwrap();

        let vars = collection.environment("prod").unwrap().variables();
        assert_eq!(vars.get("base_url").unwrap(), "https://api.test");
        assert_eq!(vars.get("api_token").unwrap(), "{{secret:prod_api_token}}");
    }

    #[test]
    fn missing_environment_lists_what_exists() {
        let dir = temp_dir();
        let collection = Collection::init(&dir, "demo").unwrap();
        let message = collection.environment("staging").unwrap_err().to_string();
        assert!(
            message.contains("dev"),
            "should suggest what's available: {message}"
        );
    }

    #[test]
    fn environment_variables_override_collection_variables() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        collection
            .manifest
            .vars
            .insert("base_url".into(), "https://default.test".into());

        let env = collection.environment("dev").unwrap();
        let vars = collection.variables_for(Some(&env));
        assert_eq!(vars.get("base_url").unwrap(), "https://httpbin.org");
    }

    #[test]
    fn defaults_fill_gaps_without_overriding_a_request() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        collection
            .manifest
            .defaults
            .headers
            .set("Accept", "application/json");
        collection
            .manifest
            .defaults
            .headers
            .set("X-Client", "gabriel");
        collection.manifest.defaults.auth = Some(Auth::Bearer {
            token: "{{secret:t}}".into(),
        });

        let mut spec = RequestSpec::new("GET", "https://x.test");
        spec.headers.set("Accept", "text/plain");
        spec.auth = Some(Auth::Inherit);

        let merged = collection.apply_defaults(&spec);
        assert_eq!(merged.headers.get_first("Accept"), Some("text/plain"));
        assert_eq!(merged.headers.get_first("X-Client"), Some("gabriel"));
        assert_eq!(
            merged.auth,
            Some(Auth::Bearer {
                token: "{{secret:t}}".into()
            })
        );
    }

    #[test]
    fn explicit_auth_survives_defaults() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        collection.manifest.defaults.auth = Some(Auth::Bearer {
            token: "default".into(),
        });

        let mut spec = RequestSpec::new("GET", "https://x.test");
        spec.auth = Some(Auth::None);
        assert_eq!(collection.apply_defaults(&spec).auth, Some(Auth::None));
    }

    #[test]
    fn saving_twice_updates_in_place() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        collection
            .save_request("x", &RequestSpec::new("GET", "https://a.test"))
            .unwrap();
        collection
            .save_request("x", &RequestSpec::new("POST", "https://b.test"))
            .unwrap();

        assert_eq!(
            collection.requests().iter().filter(|r| r.id == "x").count(),
            1
        );
        assert_eq!(collection.find("x").unwrap().spec.method, "POST");
    }

    #[test]
    fn a_request_path_cannot_escape_the_collection() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        let spec = RequestSpec::new("GET", "https://x.test");

        for attempt in [
            "../escaped",
            "../../escaped",
            "a/../../escaped",
            "/absolute/escaped",
            "..",
            "",
            "   ",
            "/",
        ] {
            let result = collection.save_request(attempt, &spec);
            assert!(
                matches!(result, Err(CollectionError::UnsafePath(_))),
                "`{attempt}` was not rejected: {result:?}"
            );
        }

        // And nothing was written outside the collection.
        assert!(!dir.join("escaped.toml").exists());
        assert!(!dir.parent().unwrap().join("escaped.toml").exists());
    }

    #[test]
    fn ordinary_nested_paths_are_still_accepted() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        let spec = RequestSpec::new("GET", "https://x.test");

        for accepted in [
            "users/create",
            "a/b/c/deep",
            "./users/list",
            "trailing/",
            "x.toml",
        ] {
            assert!(
                collection.save_request(accepted, &spec).is_ok(),
                "`{accepted}` should be allowed"
            );
        }
        assert!(
            collection
                .root()
                .join(REQUESTS_DIR)
                .join("users/create.toml")
                .is_file()
        );
        assert!(
            collection
                .root()
                .join(REQUESTS_DIR)
                .join("users/list.toml")
                .is_file()
        );
    }

    #[test]
    fn unique_paths_avoid_clobbering_existing_requests() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        assert_eq!(
            collection.unique_request_path("GET /v1/users"),
            "get-v1-users"
        );

        collection
            .save_request("get-v1-users", &RequestSpec::new("GET", "https://x.test"))
            .unwrap();
        assert_eq!(
            collection.unique_request_path("GET /v1/users"),
            "get-v1-users-2"
        );
    }

    #[test]
    fn slugify_produces_safe_file_stems() {
        assert_eq!(slugify("GET /v1/users"), "get-v1-users");
        assert_eq!(slugify("  spaces  everywhere  "), "spaces-everywhere");
        // Path separators and traversal characters cannot survive slugifying,
        // which is why the auto-derived promote path was never a traversal risk.
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
        assert_eq!(slugify("!!!"), "request");
        assert_eq!(slugify(""), "request");
        assert_eq!(slugify("café ☕"), "caf");
    }

    #[test]
    fn a_saved_request_reloads_identically() {
        let dir = temp_dir();
        let mut collection = Collection::init(&dir, "demo").unwrap();
        let mut spec = RequestSpec::new("POST", "{{base_url}}/users");
        spec.name = Some("Create".into());
        spec.headers.set("Content-Type", "application/json");
        spec.body = Some(gabriel_core::model::Body::Json {
            content: "{\"a\":1}".into(),
        });
        collection.save_request("users/create", &spec).unwrap();

        let reloaded = Collection::load(collection.root()).unwrap();
        assert_eq!(reloaded.find("users/create").unwrap().spec, spec);
    }
}
