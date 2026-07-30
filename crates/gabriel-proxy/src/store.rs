//! The capture log.
//!
//! Captures are appended as newline-delimited JSON: one line per
//! request/response pair. The format is boring on purpose — `tail -f` works,
//! `grep` works, and a truncated write costs one line rather than the file.
//!
//! Two things about it are load-bearing rather than incidental:
//!
//! * **Appends hold the file open.** Reopening per capture cost 52 µs of the
//!   72 µs an append took, on the proxy's hot path.
//! * **Reads walk backwards from the end.** Every query wants the newest
//!   captures, so decoding the whole log to answer one took 271 ms against
//!   50 000 captures. Reading in reverse makes the common case proportional to
//!   the number of rows actually asked for.

use gabriel_core::capture::Capture;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

type Result<T> = std::result::Result<T, StoreError>;

pub struct CaptureStore {
    path: PathBuf,
    /// Serialises appends from concurrently handled connections, and holds the
    /// open file between them. `None` until the first append, and reset by
    /// `clear` so a handle to an unlinked file is never written to.
    writer: Mutex<Option<std::fs::File>>,
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
        CaptureStore {
            path: path.into(),
            writer: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, capture: &Capture) -> Result<()> {
        let line = serde_json::to_string(capture).expect("capture serializes");
        let io = |source| StoreError::Io {
            path: self.path.clone(),
            source,
        };

        let mut writer = self.writer.lock().expect("write lock");
        if writer.is_none() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            *writer = Some(open_append_private(&self.path).map_err(io)?);
        }
        let file = writer.as_mut().expect("just opened");
        writeln!(file, "{line}").map_err(io)
    }

    /// Most recent first, at most `limit`. Malformed lines are skipped rather
    /// than failing the read: a half-written line must not hide the rest.
    ///
    /// Stops as soon as `limit` matches are found, so listing the last thirty
    /// captures costs thirty rows and not the whole log.
    pub fn list(&self, filter: &CaptureFilter, limit: usize) -> Result<Vec<Capture>> {
        let mut captures = Vec::new();
        if limit == 0 {
            return Ok(captures);
        }
        self.scan_backwards(|capture| {
            if filter.matches(&capture) {
                captures.push(capture);
            }
            // Keep going until we have enough.
            captures.len() < limit
        })?;
        Ok(captures)
    }

    /// Find one capture by its id, or by a unique prefix of it. Searches newest
    /// first and stops at the first match.
    pub fn get(&self, id: &str) -> Result<Option<Capture>> {
        let mut found = None;
        self.scan_backwards(|capture| {
            if capture.id == id || capture.id.starts_with(id) {
                found = Some(capture);
                return false;
            }
            true
        })?;
        Ok(found)
    }

    pub fn count(&self) -> Result<usize> {
        let mut total = 0;
        self.scan_backwards(|_| {
            total += 1;
            true
        })?;
        Ok(total)
    }

    /// Walk the log newest-first, handing each capture to `visit` until it
    /// returns false.
    ///
    /// The file is read in chunks from the end and split on newlines, so the
    /// work is proportional to what the caller consumes rather than to the size
    /// of the log.
    fn scan_backwards(&self, mut visit: impl FnMut(Capture) -> bool) -> Result<()> {
        const CHUNK: usize = 64 * 1024;

        let mut file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(StoreError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let io = |source| StoreError::Io {
            path: self.path.clone(),
            source,
        };

        let mut remaining = file.seek(SeekFrom::End(0)).map_err(io)?;
        // Bytes read but not yet consumed: the head of a line whose start lies
        // in an earlier chunk.
        let mut tail: Vec<u8> = Vec::new();

        while remaining > 0 {
            let read_size = CHUNK.min(remaining as usize);
            remaining -= read_size as u64;
            file.seek(SeekFrom::Start(remaining)).map_err(io)?;

            let mut chunk = vec![0u8; read_size];
            file.read_exact(&mut chunk).map_err(io)?;
            chunk.extend_from_slice(&tail);

            // Everything before the first newline belongs to a line that starts
            // in the previous chunk; carry it over.
            let first_newline = chunk.iter().position(|b| *b == b'\n');
            let (carry, complete) = match first_newline {
                Some(index) => (chunk[..index].to_vec(), chunk[index + 1..].to_vec()),
                // No newline in this chunk at all: the whole thing is one
                // partial line, unless we have reached the start of the file.
                None if remaining > 0 => {
                    tail = chunk;
                    continue;
                }
                None => (Vec::new(), chunk),
            };
            tail = carry;

            for line in complete.split(|b| *b == b'\n').rev() {
                if line.is_empty() {
                    continue;
                }
                if let Ok(capture) = serde_json::from_slice::<Capture>(line)
                    && !visit(capture)
                {
                    return Ok(());
                }
            }
        }

        // Whatever is left is the first line of the file.
        if !tail.is_empty()
            && let Ok(capture) = serde_json::from_slice::<Capture>(&tail)
        {
            visit(capture);
        }
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        let mut writer = self.writer.lock().expect("write lock");
        // Drop the handle first: writing to it after the file is unlinked would
        // append to an inode nothing can read.
        *writer = None;
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Io {
                path: self.path.clone(),
                source,
            }),
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
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gabriel_core::capture::{CapturedBody, CapturedRequest, CapturedResponse};
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
        store
            .append(&capture("a", "GET", "https://api.test/1", 200))
            .unwrap();
        store
            .append(&capture("b", "GET", "https://api.test/2", 200))
            .unwrap();

        let listed = store.list(&CaptureFilter::default(), 10).unwrap();
        assert_eq!(
            listed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
    }

    #[test]
    fn an_empty_log_is_not_an_error() {
        assert!(
            store()
                .list(&CaptureFilter::default(), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_corrupt_line_does_not_hide_the_others() {
        let store = store();
        store
            .append(&capture("a", "GET", "https://api.test/1", 200))
            .unwrap();
        {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(store.path())
                .unwrap();
            writeln!(file, "{{ this is not json").unwrap();
        }
        store
            .append(&capture("b", "GET", "https://api.test/2", 200))
            .unwrap();

        assert_eq!(store.list(&CaptureFilter::default(), 10).unwrap().len(), 2);
    }

    #[test]
    fn filters_by_host_method_and_status() {
        let store = store();
        store
            .append(&capture("a", "GET", "https://api.test/ok", 200))
            .unwrap();
        store
            .append(&capture("b", "POST", "https://api.test/bad", 500))
            .unwrap();
        store
            .append(&capture("c", "GET", "https://cdn.other/img", 200))
            .unwrap();

        let by_host = CaptureFilter {
            host: Some("api.test".into()),
            ..Default::default()
        };
        assert_eq!(store.list(&by_host, 10).unwrap().len(), 2);

        let by_method = CaptureFilter {
            method: Some("post".into()),
            ..Default::default()
        };
        assert_eq!(store.list(&by_method, 10).unwrap()[0].id, "b");

        let errors = CaptureFilter {
            status_min: Some(400),
            ..Default::default()
        };
        assert_eq!(store.list(&errors, 10).unwrap()[0].id, "b");
    }

    #[test]
    fn limit_takes_the_most_recent() {
        let store = store();
        for i in 0..5 {
            store
                .append(&capture(&i.to_string(), "GET", "https://api.test", 200))
                .unwrap();
        }
        let listed = store.list(&CaptureFilter::default(), 2).unwrap();
        assert_eq!(
            listed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["4", "3"]
        );
    }

    #[test]
    fn get_accepts_a_unique_prefix() {
        let store = store();
        store
            .append(&capture("cap_abc123", "GET", "https://api.test", 200))
            .unwrap();
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
        store
            .append(&capture("a", "GET", "https://api.test", 200))
            .unwrap();

        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
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

        store
            .append(&capture("a", "GET", "https://api.test", 200))
            .unwrap();

        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "stale log left readable: {mode:o}");
    }

    /// The reverse reader works in 64 KB chunks, so a log larger than that
    /// exercises the boundary logic — lines straddling a chunk edge are where
    /// this kind of code goes wrong.
    #[test]
    fn reads_correctly_across_chunk_boundaries() {
        let store = store();
        for i in 0..400 {
            store
                .append(&capture(
                    &format!("cap-{i}"),
                    "GET",
                    "https://api.test/x",
                    200,
                ))
                .unwrap();
        }
        let size = std::fs::metadata(store.path()).unwrap().len();
        assert!(
            size > 64 * 1024,
            "log too small to cross a chunk: {size} bytes"
        );

        // Newest first, and nothing skipped or duplicated at the seams.
        let listed = store.list(&CaptureFilter::default(), 5).unwrap();
        let ids: Vec<&str> = listed.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["cap-399", "cap-398", "cap-397", "cap-396", "cap-395"]
        );

        assert_eq!(
            store.count().unwrap(),
            400,
            "lines lost at a chunk boundary"
        );

        // The very first line of the file is only reached after a full walk.
        assert_eq!(store.get("cap-0").unwrap().unwrap().id, "cap-0");
        // And a line in the middle of some chunk.
        assert_eq!(store.get("cap-201").unwrap().unwrap().id, "cap-201");
    }

    #[test]
    fn every_capture_is_returned_exactly_once() {
        let store = store();
        for i in 0..250 {
            store
                .append(&capture(
                    &format!("cap-{i}"),
                    "GET",
                    "https://api.test/x",
                    200,
                ))
                .unwrap();
        }
        let all = store.list(&CaptureFilter::default(), usize::MAX).unwrap();
        let mut ids: Vec<String> = all.into_iter().map(|c| c.id).collect();
        let before = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(before, 250);
        assert_eq!(ids.len(), 250, "duplicates across chunk boundaries");
    }

    #[test]
    fn a_log_without_a_trailing_newline_still_reads() {
        let store = store();
        let line = serde_json::to_string(&capture("only", "GET", "https://api.test", 200)).unwrap();
        std::fs::write(store.path(), line).unwrap();

        let listed = store.list(&CaptureFilter::default(), 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "only");
    }

    #[test]
    fn a_single_capture_larger_than_one_chunk_is_read() {
        let store = store();
        let mut big = capture("huge", "POST", "https://api.test/upload", 200);
        big.request.body = Some(CapturedBody::Text {
            text: "x".repeat(200_000),
        });
        store.append(&big).unwrap();
        store
            .append(&capture("small", "GET", "https://api.test", 200))
            .unwrap();

        assert_eq!(store.count().unwrap(), 2);
        assert_eq!(store.get("huge").unwrap().unwrap().id, "huge");
    }

    /// The store now keeps the log open between appends, so clearing has to
    /// drop that handle — otherwise later writes land in an unlinked file.
    #[test]
    fn appending_after_a_clear_writes_to_the_new_file() {
        let store = store();
        store
            .append(&capture("first", "GET", "https://api.test", 200))
            .unwrap();
        store.clear().unwrap();
        store
            .append(&capture("second", "GET", "https://api.test", 200))
            .unwrap();

        let listed = store.list(&CaptureFilter::default(), 10).unwrap();
        assert_eq!(listed.len(), 1, "append went to a stale handle");
        assert_eq!(listed[0].id, "second");
        assert!(store.path().exists());
    }

    #[test]
    fn a_zero_limit_returns_nothing() {
        let store = store();
        store
            .append(&capture("a", "GET", "https://api.test", 200))
            .unwrap();
        assert!(store.list(&CaptureFilter::default(), 0).unwrap().is_empty());
    }

    #[test]
    fn filtering_still_finds_older_matches_beyond_the_limit() {
        let store = store();
        store
            .append(&capture("old-error", "GET", "https://api.test/a", 500))
            .unwrap();
        for i in 0..100 {
            store
                .append(&capture(
                    &format!("ok-{i}"),
                    "GET",
                    "https://api.test/b",
                    200,
                ))
                .unwrap();
        }

        let errors = CaptureFilter {
            status_min: Some(500),
            ..Default::default()
        };
        let found = store.list(&errors, 30).unwrap();
        assert_eq!(found.len(), 1, "a match older than the limit was missed");
        assert_eq!(found[0].id, "old-error");
    }

    #[test]
    fn clearing_removes_everything() {
        let store = store();
        store
            .append(&capture("a", "GET", "https://api.test", 200))
            .unwrap();
        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 0);
        // And clearing again is not an error.
        store.clear().unwrap();
    }
}
