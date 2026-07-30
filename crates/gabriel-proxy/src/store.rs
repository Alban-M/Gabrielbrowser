//! The capture log.
//!
//! Captures are appended as newline-delimited JSON: one line per
//! request/response pair. The format is boring on purpose — `tail -f` works,
//! `grep` works, and a truncated write costs one line rather than the file.

use gabriel_core::capture::Capture;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

type Result<T> = std::result::Result<T, StoreError>;

pub struct CaptureStore {
    path: PathBuf,
    /// Serialises appends from concurrently handled connections.
    write_lock: Mutex<()>,
}

/// What to include when listing captures.
#[derive(Debug, Clone, Default)]
pub struct CaptureFilter {
    /// Substring match against the host.
    pub host: Option<String>,
    /// Substring match against the full URL.
    pub url: Option<String>,
    pub method: Option<String>,
    /// Only captures whose status is in this range, e.g. `400..600` for errors.
    pub status_min: Option<u16>,
    pub status_max: Option<u16>,
    pub session: Option<String>,
}

impl CaptureFilter {
    pub fn matches(&self, capture: &Capture) -> bool {
        if let Some(host) = &self.host
            && !capture.host().contains(host.as_str())
        {
            return false;
        }
        if let Some(url) = &self.url
            && !capture.request.url.contains(url.as_str())
        {
            return false;
        }
        if let Some(method) = &self.method
            && !capture.request.method.eq_ignore_ascii_case(method)
        {
            return false;
        }
        if let Some(session) = &self.session
            && capture.session.as_deref() != Some(session.as_str())
        {
            return false;
        }
        match (capture.status(), self.status_min, self.status_max) {
            (_, None, None) => true,
            (Some(status), min, max) => {
                status >= min.unwrap_or(0) && status <= max.unwrap_or(u16::MAX)
            }
            // A capture with no response can't satisfy a status filter.
            (None, _, _) => false,
        }
    }
}

impl CaptureStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        CaptureStore { path: path.into(), write_lock: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, capture: &Capture) -> Result<()> {
        let line = serde_json::to_string(capture).expect("capture serializes");
        let _guard = self.write_lock.lock().expect("write lock");

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut file = open_append_private(&self.path)
            .map_err(|source| StoreError::Io { path: self.path.clone(), source })?;
        writeln!(file, "{line}").map_err(|source| StoreError::Io {
            path: self.path.clone(),
            source,
        })
    }

    /// Most recent first, at most `limit`. Malformed lines are skipped rather
    /// than failing the read: a half-written line must not hide the rest.
    pub fn list(&self, filter: &CaptureFilter, limit: usize) -> Result<Vec<Capture>> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(StoreError::Io { path: self.path.clone(), source }),
        };

        let mut captures: Vec<Capture> = BufReader::new(file)
            .lines()
            .map_while(std::result::Result::ok)
            .filter_map(|line| serde_json::from_str::<Capture>(&line).ok())
            .filter(|capture| filter.matches(capture))
            .collect();

        captures.reverse();
        captures.truncate(limit);
        Ok(captures)
    }

    /// Find one capture by its id, or by a unique prefix of it.
    pub fn get(&self, id: &str) -> Result<Option<Capture>> {
        let all = self.list(&CaptureFilter::default(), usize::MAX)?;
        Ok(all
            .into_iter()
            .find(|c| c.id == id || c.id.starts_with(id)))
    }

    pub fn count(&self) -> Result<usize> {
        Ok(self.list(&CaptureFilter::default(), usize::MAX)?.len())
    }

    pub fn clear(&self) -> Result<()> {
        let _guard = self.write_lock.lock().expect("write lock");
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Io { path: self.path.clone(), source }),
        }
    }
}

/// Open the log for appending with `0600`.
///
/// The capture log holds whatever the browser sent — `Cookie` and
/// `Authorization` headers included — so it is exactly as sensitive as the
/// session store and the vault, and gets the same permissions.
fn open_append_private(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        // `mode` only applies when the file is created, so tighten a log that
        // an earlier build left readable by everyone.
        let mode = file.metadata()?.permissions().mode();
        if mode & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new().create(true).append(true).open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gabriel_core::capture::{CapturedRequest, CapturedResponse};
    use gabriel_core::model::FieldMap;

    /// Tests run in parallel, so each one needs its own log file.
    fn store() -> CaptureStore {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gabriel-store-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        CaptureStore::new(dir.join("captures.ndjson"))
    }

    fn capture(id: &str, method: &str, url: &str, status: u16) -> Capture {
        Capture {
            id: id.to_string(),
            at: gabriel_core::now_ms(),
            duration_ms: 5,
            session: Some("work".into()),
            page: None,
            request: CapturedRequest {
                method: method.to_string(),
                url: url.to_string(),
                http_version: "HTTP/1.1".into(),
                headers: FieldMap::default(),
                body: None,
            },
            response: Some(CapturedResponse {
                status,
                status_text: String::new(),
                headers: FieldMap::default(),
                body: None,
            }),
        }
    }

    #[test]
    fn appends_and_lists_newest_first() {
        let store = store();
        store.append(&capture("a", "GET", "https://api.test/1", 200)).unwrap();
        store.append(&capture("b", "GET", "https://api.test/2", 200)).unwrap();

        let listed = store.list(&CaptureFilter::default(), 10).unwrap();
        assert_eq!(listed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["b", "a"]);
    }

    #[test]
    fn an_empty_log_is_not_an_error() {
        assert!(store().list(&CaptureFilter::default(), 10).unwrap().is_empty());
    }

    #[test]
    fn a_corrupt_line_does_not_hide_the_others() {
        let store = store();
        store.append(&capture("a", "GET", "https://api.test/1", 200)).unwrap();
        {
            let mut file = std::fs::OpenOptions::new().append(true).open(store.path()).unwrap();
            writeln!(file, "{{ this is not json").unwrap();
        }
        store.append(&capture("b", "GET", "https://api.test/2", 200)).unwrap();

        assert_eq!(store.list(&CaptureFilter::default(), 10).unwrap().len(), 2);
    }

    #[test]
    fn filters_by_host_method_and_status() {
        let store = store();
        store.append(&capture("a", "GET", "https://api.test/ok", 200)).unwrap();
        store.append(&capture("b", "POST", "https://api.test/bad", 500)).unwrap();
        store.append(&capture("c", "GET", "https://cdn.other/img", 200)).unwrap();

        let by_host = CaptureFilter { host: Some("api.test".into()), ..Default::default() };
        assert_eq!(store.list(&by_host, 10).unwrap().len(), 2);

        let by_method = CaptureFilter { method: Some("post".into()), ..Default::default() };
        assert_eq!(store.list(&by_method, 10).unwrap()[0].id, "b");

        let errors = CaptureFilter { status_min: Some(400), ..Default::default() };
        assert_eq!(store.list(&errors, 10).unwrap()[0].id, "b");
    }

    #[test]
    fn limit_takes_the_most_recent() {
        let store = store();
        for i in 0..5 {
            store.append(&capture(&i.to_string(), "GET", "https://api.test", 200)).unwrap();
        }
        let listed = store.list(&CaptureFilter::default(), 2).unwrap();
        assert_eq!(listed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["4", "3"]);
    }

    #[test]
    fn get_accepts_a_unique_prefix() {
        let store = store();
        store.append(&capture("cap_abc123", "GET", "https://api.test", 200)).unwrap();
        assert_eq!(store.get("cap_abc").unwrap().unwrap().id, "cap_abc123");
        assert!(store.get("nope").unwrap().is_none());
    }

    /// The log records `Cookie` and `Authorization` headers verbatim. Anything
    /// less than `0600` hands every local account the developer's sessions.
    #[cfg(unix)]
    #[test]
    fn the_capture_log_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt as _;
        let store = store();
        store.append(&capture("a", "GET", "https://api.test", 200)).unwrap();

        let mode = std::fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "capture log readable by others: {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_permissive_log_is_tightened_on_the_next_append() {
        use std::os::unix::fs::PermissionsExt as _;
        let store = store();
        // Simulate a log left behind by a build that wrote 0644.
        std::fs::write(store.path(), "").unwrap();
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();

        store.append(&capture("a", "GET", "https://api.test", 200)).unwrap();

        let mode = std::fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "stale log left readable: {mode:o}");
    }

    #[test]
    fn clearing_removes_everything() {
        let store = store();
        store.append(&capture("a", "GET", "https://api.test", 200)).unwrap();
        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 0);
        // And clearing again is not an error.
        store.clear().unwrap();
    }
}
