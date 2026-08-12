//! Where the file is, how it is found, and how it is written to safely.

use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::event::{self, Event};
use crate::state::State;

pub const FILE_NAME: &str = ".podrick";
pub const ARCHIVE_NAME: &str = ".podrick.archive";
pub const LOCK_NAME: &str = ".podrick.lock";
const LOCK_TIMEOUT_SECS: u64 = 5;
/// 32 symbols: the alphabet without `l`/`o` and the digits without `0`/`1`, so an id is
/// never misread aloud or mistyped.
const ID_ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";

// ---------------------------------------------------------------------------
// Locations
// ---------------------------------------------------------------------------

pub fn data_dir() -> Result<PathBuf> {
    let d = dirs::data_dir()
        .ok_or_else(|| AppError::io("no data directory on this platform"))?
        .join("podrick");
    fs::create_dir_all(&d)?;
    Ok(d)
}

pub fn config_dir() -> Result<PathBuf> {
    let d = dirs::config_dir()
        .ok_or_else(|| AppError::io("no config directory on this platform"))?
        .join("podrick");
    fs::create_dir_all(&d)?;
    Ok(d)
}

pub fn global_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("global.podrick"))
}

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub fn git_root(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

pub fn git_remote(dir: &Path) -> Option<String> {
    git(dir, &["remote", "get-url", "origin"]).map(|u| normalize_remote(&u))
}

/// `git@github.com:a/b.git` and `https://github.com/a/b` are the same repo.
fn normalize_remote(url: &str) -> String {
    let u = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let u = u.strip_prefix("git@").unwrap_or(u);
    let u = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .or_else(|| u.strip_prefix("ssh://"))
        .unwrap_or(u);
    u.replacen(':', "/", 1).to_lowercase()
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegEntry {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
    /// Per-project sort override. Config, not history — so it lives here, not in the log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    /// Last time we nudged about compaction, so it stays at most daily.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_nudge: Option<String>,
}

#[derive(Debug, Default)]
pub struct Registry {
    pub entries: Vec<RegEntry>,
}

impl Registry {
    fn file() -> Result<PathBuf> {
        Ok(data_dir()?.join("registry.jsonl"))
    }

    pub fn load() -> Result<Registry> {
        let f = Self::file()?;
        let Ok(contents) = fs::read_to_string(&f) else {
            return Ok(Registry::default());
        };
        let mut entries: Vec<RegEntry> = contents
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        // Entries whose file is gone are pruned silently.
        entries.retain(|e| e.path.exists());
        Ok(Registry { entries })
    }

    pub fn save(&self) -> Result<()> {
        let f = Self::file()?;
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&serde_json::to_string(e).map_err(|e| AppError::io(e.to_string()))?);
            out.push('\n');
        }
        fs::write(f, out)?;
        Ok(())
    }

    pub fn get(&self, path: &Path) -> Option<&RegEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    pub fn upsert(&mut self, path: &Path, now: &str) -> &mut RegEntry {
        let remote = path.parent().and_then(git_remote);
        if let Some(i) = self.entries.iter().position(|e| e.path == path) {
            self.entries[i].last_used = Some(now.to_string());
            if self.entries[i].remote.is_none() {
                self.entries[i].remote = remote;
            }
            return &mut self.entries[i];
        }
        self.entries.push(RegEntry {
            path: path.to_path_buf(),
            remote,
            last_used: Some(now.to_string()),
            sort: None,
            last_nudge: None,
        });
        self.entries.last_mut().expect("just pushed")
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// `-f <path>`
    Explicit,
    /// `-g`
    Global,
    /// Found by walking up from cwd.
    WalkUp,
    /// Matched from the registry on a strong signal and adopted.
    Adopted(Signal),
    /// Newly created.
    Created,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// S1 — cwd is inside the file's tree, or contains it.
    SameTree,
    /// S2 — same git remote (catches worktrees and second clones).
    SameRemote,
}

impl Signal {
    pub fn describe(self) -> &'static str {
        match self {
            Signal::SameTree => "same directory tree",
            Signal::SameRemote => "same git remote",
        }
    }
}

/// A weak match — surfaced as a suggestion, never acted on.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub path: PathBuf,
    pub why: &'static str,
}

pub struct Resolved {
    pub path: PathBuf,
    pub how: How,
    pub suggestions: Vec<Suggestion>,
}

pub struct ResolveOpts<'a> {
    pub explicit: Option<&'a Path>,
    pub global: bool,
    pub here: bool,
    /// Whether the command is allowed to create a file that does not exist.
    pub may_create: bool,
    pub cwd: PathBuf,
}

/// Find the task file for this invocation. See SPEC §2.3.
///
/// Returns `Ok(None)` when no file exists and creation was not permitted — the caller
/// decides whether to prompt (TTY) or refuse (agent).
pub fn resolve(opts: &ResolveOpts, reg: &Registry) -> Result<Option<Resolved>> {
    if let Some(p) = opts.explicit {
        return Ok(Some(Resolved {
            path: p.to_path_buf(),
            how: How::Explicit,
            suggestions: vec![],
        }));
    }
    if opts.global {
        return Ok(Some(Resolved {
            path: global_file()?,
            how: How::Global,
            suggestions: vec![],
        }));
    }

    let home = dirs::home_dir();
    let root = git_root(&opts.cwd);

    // --here skips discovery entirely: this project, nowhere else.
    if opts.here {
        let dir = root.clone().unwrap_or_else(|| opts.cwd.clone());
        let path = dir.join(FILE_NAME);
        let how = if path.exists() {
            How::WalkUp
        } else {
            How::Created
        };
        return Ok(Some(Resolved {
            path,
            how,
            suggestions: vec![],
        }));
    }

    // Walk up from cwd, stopping at the git root or $HOME, whichever comes first.
    let mut cur = Some(opts.cwd.as_path());
    while let Some(dir) = cur {
        let candidate = dir.join(FILE_NAME);
        if candidate.exists() {
            return Ok(Some(Resolved {
                path: candidate,
                how: How::WalkUp,
                suggestions: vec![],
            }));
        }
        if root.as_deref() == Some(dir) || home.as_deref() == Some(dir) {
            break;
        }
        cur = dir.parent();
    }

    // Nothing on the way up — consult the registry.
    let (strong, weak) = match_registry(&opts.cwd, root.as_deref(), reg);
    if let Some((path, signal)) = strong {
        return Ok(Some(Resolved {
            path,
            how: How::Adopted(signal),
            suggestions: weak,
        }));
    }

    if !opts.may_create {
        return Ok(None);
    }

    let dir = root.unwrap_or_else(|| opts.cwd.clone());
    Ok(Some(Resolved {
        path: dir.join(FILE_NAME),
        how: How::Created,
        suggestions: weak,
    }))
}

/// Rank registry entries against cwd. Returns the strong match, if any, plus weak
/// suggestions that are only ever mentioned.
fn match_registry(
    cwd: &Path,
    root: Option<&Path>,
    reg: &Registry,
) -> (Option<(PathBuf, Signal)>, Vec<Suggestion>) {
    let mut strong: Option<(PathBuf, Signal)> = None;
    let mut weak = Vec::new();

    let remote = git_remote(cwd);
    let cwd_base = cwd.file_name().map(|s| s.to_string_lossy().to_lowercase());

    for e in &reg.entries {
        let Some(dir) = e.path.parent() else { continue };

        // S1 — cwd inside the entry's tree, or the entry inside cwd's tree.
        if cwd.starts_with(dir) || dir.starts_with(cwd) {
            strong.get_or_insert((e.path.clone(), Signal::SameTree));
            continue;
        }

        // S2 — same git remote. Catches worktrees and second clones; correctly keeps
        // list.am and list.am-mobile apart, since their remotes differ.
        if let (Some(r), Some(er)) = (&remote, &e.remote) {
            if r == er {
                strong.get_or_insert((e.path.clone(), Signal::SameRemote));
                continue;
            }
        }

        // S3 — sibling package of the same monorepo.
        if let Some(root) = root {
            if dir.starts_with(root) {
                weak.push(Suggestion {
                    path: e.path.clone(),
                    why: "same repository",
                });
                continue;
            }
        }

        // S4 — matching directory basename.
        if let (Some(a), Some(b)) = (&cwd_base, dir.file_name()) {
            if *a == b.to_string_lossy().to_lowercase() {
                weak.push(Suggestion {
                    path: e.path.clone(),
                    why: "matching directory name",
                });
            }
        }
    }

    // S5 — most recently used, when nothing better turned up.
    if strong.is_none() && weak.is_empty() {
        if let Some(e) = reg
            .entries
            .iter()
            .filter(|e| e.last_used.is_some())
            .max_by(|a, b| a.last_used.cmp(&b.last_used))
        {
            weak.push(Suggestion {
                path: e.path.clone(),
                why: "most recently used",
            });
        }
    }

    (strong, weak)
}

// ---------------------------------------------------------------------------
// Reading and writing
// ---------------------------------------------------------------------------

pub struct Store {
    pub path: PathBuf,
    pub state: State,
    pub torn_lines: usize,
    /// How long the replay took, for the compaction nudge.
    pub replay_micros: u128,
}

pub fn load(path: &Path) -> Result<Store> {
    let started = std::time::Instant::now();
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let (events, torn) = event::parse_log(&contents);
    let mut state = State::replay(&events);
    state.torn_lines = torn;
    Ok(Store {
        path: path.to_path_buf(),
        state,
        torn_lines: torn,
        replay_micros: started.elapsed().as_micros(),
    })
}

pub fn read_events(path: &Path) -> Result<Vec<Event>> {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    Ok(event::parse_log(&contents).0)
}

/// An advisory lock held for the duration of a write. A torn ledger is the one failure
/// this tool cannot recover from, so every write pays for this.
pub struct Lock {
    file: File,
    path: PathBuf,
}

impl Lock {
    pub fn acquire(target: &Path) -> Result<Lock> {
        let dir = target.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir)?;
        let path = dir.join(LOCK_NAME);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)?;

        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(LOCK_TIMEOUT_SECS);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Lock { file, path }),
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(_) => {
                    // Break a stale lock rather than wedge forever, but say so.
                    eprintln!(
                        "podrick: lock at {} held for over {LOCK_TIMEOUT_SECS}s; proceeding anyway",
                        path.display()
                    );
                    return Ok(Lock { file, path });
                }
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        let _ = self.path.metadata(); // keep the file; unlinking races other holders
    }
}

/// Append events to the log. Serialised and written in a single call under the lock.
pub fn append(path: &Path, events: &[Event]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let _lock = Lock::acquire(path)?;
    let mut buf = String::new();
    for e in events {
        buf.push_str(&serde_json::to_string(e).map_err(|e| AppError::io(e.to_string()))?);
        buf.push('\n');
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(buf.as_bytes())?;
    f.flush()?;
    Ok(())
}

/// Move the current log into the archive and start fresh from a snapshot.
pub fn compact(path: &Path, snapshot_event: &Event) -> Result<usize> {
    let _lock = Lock::acquire(path)?;

    let mut contents = String::new();
    if let Ok(mut f) = File::open(path) {
        f.read_to_string(&mut contents)?;
    }
    let line_count = contents.lines().filter(|l| !l.trim().is_empty()).count();

    if !contents.is_empty() {
        let archive = path.with_file_name(ARCHIVE_NAME);
        let mut a = OpenOptions::new().create(true).append(true).open(archive)?;
        a.write_all(contents.as_bytes())?;
        a.flush()?;
    }

    let line = serde_json::to_string(snapshot_event).map_err(|e| AppError::io(e.to_string()))?;
    fs::write(path, format!("{line}\n"))?;
    Ok(line_count)
}

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

/// A short permanent id. Ambiguous glyphs (l, 1, 0, o) are left out of the alphabet.
pub fn new_id(taken: &dyn Fn(&str) -> bool) -> String {
    for _ in 0..10_000 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(
            std::time::SystemTime::now()
                .elapsed()
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        );
        let mut n = h.finish();
        let mut id = String::with_capacity(3);
        for _ in 0..3 {
            id.push(ID_ALPHABET[(n % ID_ALPHABET.len() as u64) as usize] as char);
            n /= ID_ALPHABET.len() as u64;
        }
        if !taken(&id) {
            return id;
        }
    }
    // Alphabet exhausted at 3 chars — widen rather than fail.
    let mut id = String::new();
    let mut h = RandomState::new().build_hasher();
    h.write_u64(0);
    let mut n = h.finish();
    for _ in 0..6 {
        id.push(ID_ALPHABET[(n % ID_ALPHABET.len() as u64) as usize] as char);
        n /= ID_ALPHABET.len() as u64;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_normalize_across_protocols() {
        assert_eq!(
            normalize_remote("git@github.com:aram-p/podrick.git"),
            normalize_remote("https://github.com/aram-p/podrick")
        );
        assert_eq!(
            normalize_remote("https://github.com/aram-p/podrick/"),
            "github.com/aram-p/podrick"
        );
    }

    #[test]
    fn different_repos_do_not_collide() {
        assert_ne!(
            normalize_remote("git@github.com:x/list.am.git"),
            normalize_remote("git@github.com:x/list.am-mobile.git")
        );
    }

    #[test]
    fn ids_avoid_ambiguous_glyphs_and_collisions() {
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..200 {
            let taken = |s: &str| seen.iter().any(|x| x == s);
            let id = new_id(&taken);
            assert_eq!(id.len(), 3);
            assert!(
                !id.contains(['l', '1', '0', 'o']),
                "ambiguous glyph in {id}"
            );
            assert!(!seen.contains(&id));
            seen.push(id);
        }
    }
}
