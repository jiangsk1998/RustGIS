//! Native disposable caches. A directory lock serializes publication and eviction;
//! per-entry leases protect readers/builders across processes, including PID reuse.
use std::collections::{BTreeSet, BinaryHeap};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MIB: u64 = 1024 * 1024;
const CANDIDATES: usize = 256;
pub const INDEX_DEFAULT_BYTES: u64 = 2048 * MIB;

pub fn index_budget() -> u64 {
    std::env::var("RUSTGIS_SHP_CACHE_MIB").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(INDEX_DEFAULT_BYTES / MIB).clamp(64, 65536)
        * MIB
}
pub const INDEX_DEFAULT_DAYS: u64 = 30;
/// How long an index may go unused before a sweep drops it, within budget or not: indexes of
/// files that were deleted or moved are never opened again (#1230).
pub fn index_max_idle() -> Duration {
    let days =
        std::env::var("RUSTGIS_SHP_CACHE_DAYS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(INDEX_DEFAULT_DAYS).clamp(1, 3650);
    Duration::from_secs(days * 24 * 60 * 60)
}
pub fn index_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = TEST_INDEX_DIR.with(|root| root.borrow().clone()) {
        return root;
    }
    if let Some(root) = PROCESS_INDEX_DIR.get() {
        return root.clone();
    }
    // The crate's own tests never fill the user's cache (#1230), unless a test names a folder or
    // `RUSTGIS_SHP_CACHE` does (the cross-process tests set it).
    #[cfg(test)]
    if std::env::var_os("RUSTGIS_SHP_CACHE").filter(|v| !v.is_empty()).is_none() {
        return use_temporary_index_dir();
    }
    crate::source::external::cache_dir("RUSTGIS_SHP_CACHE", "shp-index").unwrap_or_else(index_fallback_dir)
}

static PROCESS_INDEX_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Keeps this process's Shapefile indexes out of the user's cache: they go to a folder of its
/// own in the system temporary directory, which it returns, even when `RUSTGIS_SHP_CACHE` is
/// set. For test suites, which would otherwise add an index to the user's cache for every
/// Shapefile they write (#1230). The folder is per process, so a suite's cache never grows from
/// one run to the next; the system clears its temporary directory.
pub fn use_temporary_index_dir() -> PathBuf {
    PROCESS_INDEX_DIR
        .get_or_init(|| {
            #[cfg(unix)]
            let user = rustix::process::geteuid().as_raw().to_string();
            #[cfg(not(unix))]
            let user = "user";
            std::env::temp_dir().join(format!("rustgis-shp-index-test-{user}-{}", std::process::id()))
        })
        .clone()
}

// Test-local routing avoids process-wide environment mutation and interference with
// parallel tests or the user's persistent indexes. Production always uses the path above.
#[cfg(test)]
thread_local! {
    static TEST_INDEX_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) fn with_index_dir<T>(root: PathBuf, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_INDEX_DIR.with(|root| *root.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(TEST_INDEX_DIR.with(|current| current.replace(Some(root))));
    run()
}
/// Separate Unix users even when the system temporary directory is shared.
pub fn index_fallback_dir() -> PathBuf {
    #[cfg(unix)]
    let name = format!("rustgis-shp-index-{}", rustix::process::geteuid().as_raw());
    #[cfg(not(unix))]
    let name = "rustgis-shp-index";
    std::env::temp_dir().join(name)
}

/// A lock file whose advisory lock is released explicitly when it is dropped.
///
/// Closing a descriptor releases its `flock` only once every duplicate of it is
/// closed, and a process spawned on another thread (GDAL, DuckDB, a plugin) holds
/// duplicates of all descriptors until it execs. Dropping a plain `File` could
/// then leave a lease or the directory lock held for that window, so a sweep or a
/// release that runs meanwhile finds the entry pinned and leaves it behind.
/// Unlocking acts on the shared open file description, duplicates included, and
/// is a no-op for a lock that was never taken.
#[derive(Debug)]
struct Flock(File);
impl std::ops::Deref for Flock {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}
impl std::ops::DerefMut for Flock {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.0
    }
}
impl Drop for Flock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Never follow a lease symlink or open a special file, including after a rename race.
fn lease_file(path: &Path, create: bool) -> io::Result<Flock> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(io::Error::other("cache lease is not a regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("cache lease is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "cache lease is not private to this user"));
        }
    }
    Ok(Flock(file))
}
fn open_lock(path: &Path) -> io::Result<Flock> {
    lease_file(path, true)
}

/// Resolve once, validate the actual storage, and never use an unstable alias again.
fn validate_directory(root: &Path) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = rustix::process::geteuid().as_raw();
        // Strip trailing slash/dot spellings before lstat: those can otherwise follow
        // a final symlink even though symlink_metadata normally does not follow it.
        let absolute: PathBuf = std::path::absolute(root)?.components().collect();
        let metadata = std::fs::symlink_metadata(&absolute)?;
        if !metadata.file_type().is_dir() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "cache root is not private to this user"));
        }
        let resolved = absolute.canonicalize()?;
        for (depth, parent) in resolved.ancestors().enumerate() {
            let metadata = std::fs::symlink_metadata(parent)?;
            let untrusted_owner = metadata.uid() != uid && (depth == 0 || metadata.uid() != 0);
            let writable = metadata.mode() & 0o022 != 0;
            if !metadata.file_type().is_dir() || untrusted_owner || (writable && (depth == 0 || metadata.mode() & 0o1000 == 0)) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "cache path is not trusted"));
            }
        }
        Ok(resolved)
    }
    #[cfg(not(unix))]
    {
        // Preserve the platform's existing directory/junction behavior outside Unix.
        if !std::fs::metadata(root)?.is_dir() {
            return Err(io::Error::other("cache root is not a directory"));
        }
        Ok(root.to_path_buf())
    }
}

#[derive(Debug)]
struct Directory {
    root: PathBuf,
    file: Flock,
}

/// Shared validation for blocking checkout and nonblocking release paths.
fn directory_file(root: &Path) -> io::Result<Directory> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(root)?;
    let root = validate_directory(root)?;
    let file = open_lock(&root.join(".rustgis-cache-v1.lock"))?;
    Ok(Directory { root, file })
}
fn directory_lock(root: &Path) -> io::Result<Directory> {
    let directory = directory_file(root)?;
    directory.file.lock()?;
    Ok(directory)
}
fn lease_path(path: &Path) -> PathBuf {
    path.with_extension("lease")
}
fn state(file: &mut File) -> io::Result<u8> {
    file.rewind()?;
    let mut value = [0u8];
    file.read_exact(&mut value)?;
    Ok(value[0])
}
fn mark(file: &mut File, value: u8) -> io::Result<()> {
    file.rewind()?;
    file.write_all(&[value])?;
    file.set_len(1)?;
    file.flush()?;
    file.set_modified(SystemTime::now())
}
fn unique() -> String {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}
fn regular(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| {
        if !m.file_type().is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if m.uid() != rustix::process::geteuid().as_raw() || m.mode() & 0o022 != 0 {
                return false;
            }
        }
        true
    })
}
fn owned(name: &str) -> bool {
    if let Some(key) = name.strip_prefix("rg-index-v1-").and_then(|s| s.strip_suffix(".sqlite")) {
        return key.len() == 33 && key.bytes().enumerate().all(|(i, c)| if i == 16 { c == b'-' } else { c.is_ascii_hexdigit() });
    }
    let id = name.strip_prefix("rg-index-work-v1-").and_then(|s| s.strip_suffix(".sqlite"));
    let Some(id) = id else { return false };
    let mut parts = id.split('-');
    parts.next().is_some_and(|s| s.parse::<u32>().is_ok_and(|p| p > 0))
        && parts.next().is_some_and(|s| s.parse::<u128>().is_ok())
        && parts.next().is_some_and(|s| s.parse::<u64>().is_ok())
        && parts.next().is_none()
}
fn existing_lock(path: &Path) -> io::Result<Flock> {
    if !regular(path) {
        return Err(io::Error::other("missing regular cache lease"));
    }
    lease_file(path, false)
}

/// Kept after the SQLite connection. The sidecar stays locked for the
/// whole lifetime, rather than just during I/O. No PID-based assumption for new caches.
pub struct Lease {
    path: PathBuf,
    lock: Option<Flock>,
    temporary: bool,
    ready: bool,
    budget: u64,
}
impl Lease {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn commit(&mut self) -> io::Result<()> {
        if !self.temporary && self.path.metadata()?.len() > self.budget {
            self.temporary = true;
        }
        let root = self.path.parent().ok_or_else(|| io::Error::other("missing cache directory"))?;
        let directory = directory_lock(root)?;
        // SQLite uses disposable build settings. Flush its completed pages before
        // recording readiness, so a power loss cannot publish an unflushed index.
        if !self.temporary {
            File::options().read(true).write(true).open(&self.path)?.sync_all()?;
        }
        let file = self.lock.as_mut().ok_or_else(|| io::Error::other("missing cache lease"))?;
        mark(file, if self.temporary { b'T' } else { b'C' })?;
        if !self.temporary {
            file.sync_all()?;
        }
        // The directory lock closes the downgrade/open/eviction race on Unix as well.
        file.unlock()?;
        file.lock_shared()?;
        self.ready = true;
        drop(directory);
        // A new index of a file replaces those of its earlier versions (#1230).
        let newest = if self.temporary { None } else { self.path.file_name().and_then(|n| n.to_str()) };
        if let Ok(directory) = directory_lock(root) {
            let _ = prune_locked(&directory.root, self.budget, newest);
        }
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let Some(root) = self.path.parent() else { return };
        // A release during another sweep must not deadlock. That sweep, the next
        // checkout, or the next startup will reclaim any now-unlocked temporary file.
        if self.ready
            && !self.temporary
            && let Some(file) = &self.lock
        {
            let _ = file.set_modified(SystemTime::now());
        }
        self.lock.take();
        let Ok(directory) = directory_file(root) else { return };
        if directory.file.try_lock().is_err() {
            retry_maintenance(root, self.budget);
            return;
        }
        if self.temporary || !self.ready {
            let _ = remove_unpinned(&self.path);
        }
        // Sweep only a cache the last sweep left over budget with indexes in use, which a release
        // may free (#1230): a sweep per release made every open slower as the cache grew. A
        // failed build never reached the sweep at commit, so it sweeps here: abandoned builds
        // filling the disk would otherwise make every later build fail too.
        if !self.ready || over_budget(&directory.root) {
            let _ = prune_locked(&directory.root, self.budget, None);
        }
    }
}

/// The cache folders whose last sweep in this process ended over budget with indexes in use, or
/// failed. Only this process's sweeps set or clear the mark: one in another process doesn't
/// reach it until this process builds an index and sweeps again.
static OVER_BUDGET: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

fn over_budget(root: &Path) -> bool {
    OVER_BUDGET.lock().unwrap_or_else(|e| e.into_inner()).contains(root)
}

fn note_budget(root: &Path, over: bool) {
    let mut roots = OVER_BUDGET.lock().unwrap_or_else(|e| e.into_inner());
    if over {
        roots.insert(root.to_path_buf());
    } else {
        roots.remove(root);
    }
}

#[cfg(test)]
thread_local! {
    static SWEEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Full sweeps of the cache folder run on this thread so far (tests only).
#[cfg(test)]
pub(crate) fn sweeps() -> usize {
    SWEEPS.with(std::cell::Cell::get)
}

// Releases racing a sweep need one later pass: the sweep may already have counted
// that entry as pinned. Coalesce roots on one bounded worker, never one thread per drop.
fn retry_maintenance(root: &Path, budget: u64) {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Condvar, OnceLock};
    type Jobs = Arc<(Mutex<BTreeMap<PathBuf, u64>>, Condvar)>;
    static WORKER: OnceLock<Option<Jobs>> = OnceLock::new();
    let jobs = WORKER.get_or_init(|| {
        let jobs: Jobs = Arc::new((Mutex::new(BTreeMap::new()), Condvar::new()));
        let worker = jobs.clone();
        std::thread::Builder::new()
            .name("rustgis-cache-retry".into())
            .spawn(move || {
                loop {
                    let job = {
                        let (queue, wake) = &*worker;
                        let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
                        while queue.is_empty() {
                            queue = wake.wait(queue).unwrap_or_else(|e| e.into_inner());
                        }
                        queue.pop_first()
                    };
                    if let Some((root, budget)) = job
                        && root.is_dir()
                    {
                        let _ = maintain(&root, budget);
                    }
                }
            })
            .ok()
            .map(|_| jobs)
    });
    if let Some(jobs) = jobs {
        let (queue, wake) = &**jobs;
        let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
        if queue.contains_key(root) || queue.len() < 64 {
            queue.entry(root.to_path_buf()).and_modify(|b| *b = (*b).min(budget)).or_insert(budget);
            wake.notify_one();
        }
    }
}

/// Called under the directory lock; excludes symlinks and active readers/builders.
fn remove_unpinned(path: &Path) -> io::Result<bool> {
    if !regular(path) {
        return Ok(false);
    }
    let sidecar = lease_path(path);
    let lock = existing_lock(&sidecar)?;
    if lock.try_lock().is_err() {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    drop(lock);
    let _ = std::fs::remove_file(sidecar);
    Ok(true)
}

fn create_locked(path: PathBuf, temporary: bool, budget: u64) -> io::Result<(Lease, File)> {
    let mut lock = open_lock(&lease_path(&path))?;
    lock.try_lock().map_err(io::Error::other)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Establish ownership before creating the payload. A missing or invalid lease
    // never authorizes deleting a similarly named unrelated file during a sweep.
    mark(&mut lock, if temporary { b'T' } else { b'B' })?;
    let file = options.open(&path)?;
    Ok((Lease { path, lock: Some(lock), temporary, ready: false, budget }, file))
}

/// Acquire a ready index before opening SQLite. Only names from this protocol are managed.
pub fn pin_index(root: &Path, key: &str, budget: u64) -> io::Result<Option<Lease>> {
    let directory = directory_lock(root)?;
    let path = index_path(&directory.root, key)?;
    if !regular(&path) {
        return Ok(None);
    }
    let mut lock = open_lock(&lease_path(&path))?;
    if lock.try_lock_shared().is_err() || state(&mut lock).ok() != Some(b'C') {
        return Ok(None);
    }
    lock.set_modified(SystemTime::now())?;
    Ok(Some(Lease { path, lock: Some(lock), temporary: false, ready: true, budget }))
}
fn index_path(root: &Path, key: &str) -> io::Result<PathBuf> {
    if key.len() != 33 || !key.bytes().enumerate().all(|(i, c)| if i == 16 { c == b'-' } else { c.is_ascii_hexdigit() }) {
        return Err(io::Error::other("invalid cache key"));
    }
    Ok(root.join(format!("rg-index-v1-{key}.sqlite")))
}
pub fn reserve_index(root: &Path, key: &str, budget: u64) -> io::Result<Lease> {
    let directory = directory_lock(root)?;
    let path = index_path(&directory.root, key)?;
    let reserved = reserve_locked(&directory.root, path, budget);
    if reserved.is_err() {
        // No lease was taken here, so no release will sweep this folder: sweep it now, so
        // abandoned builds filling the disk don't make every later reservation fail too.
        let _ = prune_locked(&directory.root, budget, None);
    }
    reserved
}
fn reserve_locked(root: &Path, path: PathBuf, budget: u64) -> io::Result<Lease> {
    // A valid generation pinned by another reader is never replaced. Concurrent
    // builders can use a private temporary index without losing functionality.
    let mut lock = open_lock(&lease_path(&path))?;
    let available = lock.try_lock().is_ok();
    let managed = available && matches!(state(&mut lock).ok(), Some(b'C' | b'B' | b'T'));
    if available && (!path.exists() || (managed && regular(&path) && std::fs::remove_file(&path).is_ok())) {
        drop(lock);
        let (lease, file) = create_locked(path, false, budget)?;
        drop(file);
        return Ok(lease);
    }
    drop(lock);
    let (lease, file) = create_locked(root.join(format!("rg-index-work-v1-{}.sqlite", unique())), true, budget)?;
    drop(file);
    Ok(lease)
}

#[derive(Default, Debug)]
pub struct PruneStats {
    pub removed: u64,
    pub removed_bytes: u64,
    pub remaining_bytes: u64,
    /// Indexes in use (by this process or another) left in place, and their bytes.
    pub pinned: u64,
    pub pinned_bytes: u64,
}
/// What emptying the Shapefile index cache did (#1259).
#[derive(Default, Debug)]
#[non_exhaustive]
pub struct Emptied {
    /// Indexes removed, and their bytes.
    pub removed: u64,
    pub removed_bytes: u64,
    /// Indexes in use (by this process or another) that stayed, and their bytes.
    pub in_use: u64,
    pub in_use_bytes: u64,
    /// Bytes that stayed although not in use: files without a readable lease, or that the system
    /// refused to remove.
    pub kept_bytes: u64,
    /// The folders emptied, and those that couldn't be, with why.
    pub folders: u64,
    pub failed: Vec<String>,
}

/// Removes every Shapefile index not in use, in the cache folder and its fallback, and says what
/// it freed, what stayed and which folder it couldn't empty (#1259). Indexes an open layer or
/// another RustGIS instance is reading keep their leases, so they stay. A folder that doesn't
/// exist is skipped; one that can't be emptied doesn't stop the other.
pub fn empty_index_cache() -> Emptied {
    // A process with a folder of its own (a test suite) never empties the shared fallback.
    if cfg!(test) || PROCESS_INDEX_DIR.get().is_some() {
        return empty_folders(&[index_dir()]);
    }
    let (primary, fallback) = (index_dir(), index_fallback_dir());
    // The same folder spelled two ways (a symlinked temporary directory) is emptied once.
    let same = primary == fallback || matches!((primary.canonicalize(), fallback.canonicalize()), (Ok(a), Ok(b)) if a == b);
    empty_folders(&if same { vec![primary] } else { vec![primary, fallback] })
}
fn empty_folders(roots: &[PathBuf]) -> Emptied {
    let mut emptied = Emptied::default();
    for root in roots {
        match std::fs::metadata(root) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                emptied.failed.push(format!("{}: not a folder", root.display()));
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                emptied.failed.push(format!("{}: {e}", root.display()));
                continue;
            }
        }
        // A budget of 0 evicts everything not in use. With only indexes in use left, the folder
        // stays marked over budget until the next release sweeps it at the usual budget.
        match maintain(root, 0) {
            Ok(stats) => {
                emptied.folders += 1;
                emptied.removed += stats.removed;
                emptied.removed_bytes = emptied.removed_bytes.saturating_add(stats.removed_bytes);
                emptied.in_use += stats.pinned;
                emptied.in_use_bytes = emptied.in_use_bytes.saturating_add(stats.pinned_bytes);
                emptied.kept_bytes = emptied.kept_bytes.saturating_add(stats.remaining_bytes.saturating_sub(stats.pinned_bytes));
            }
            Err(e) => emptied.failed.push(format!("{}: {e}", root.display())),
        }
    }
    emptied
}
pub fn maintain(root: &Path, budget: u64) -> io::Result<PruneStats> {
    let directory = directory_lock(root)?;
    prune_locked(&directory.root, budget, None)
}
/// Sweeps a locked cache folder. `newest` names an index just committed: idle indexes of
/// earlier versions of the same file go with it.
fn prune_locked(root: &Path, budget: u64, newest: Option<&str>) -> io::Result<PruneStats> {
    #[cfg(test)]
    SWEEPS.with(|n| n.set(n.get() + 1));
    let stats = sweep(root, budget, newest);
    // Mark the folder for the next release only when releases can bring it back within budget:
    // a cache over budget because of indexes in use, or a sweep that failed. Files a sweep never
    // evicts (without a readable lease) would otherwise make every release sweep, and evict the
    // index it just released.
    let over = match &stats {
        Ok(s) => s.remaining_bytes > budget && s.pinned_bytes > 0 && s.remaining_bytes.saturating_sub(s.pinned_bytes) <= budget,
        Err(_) => true,
    };
    note_budget(root, over);
    stats
}
fn sweep(root: &Path, budget: u64, newest: Option<&str>) -> io::Result<PruneStats> {
    // `rg-index-v1-{file}-{version}.sqlite`: the same file's other versions share the prefix.
    let replaced = newest.and_then(|name| name.get(..29)).filter(|prefix| prefix.starts_with("rg-index-v1-"));
    let now = SystemTime::now();
    let max_idle = index_max_idle();
    let mut stats = PruneStats::default();
    loop {
        stats.remaining_bytes = 0;
        stats.pinned = 0;
        stats.pinned_bytes = 0;
        let removed_before = stats.removed;
        // Bounded metadata even if a cache folder contains millions of files.
        let mut oldest = BinaryHeap::new();
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".lease") {
                let payload = root.join(format!("{stem}.sqlite"));
                if owned(&payload.file_name().unwrap_or_default().to_string_lossy())
                    && std::fs::symlink_metadata(&payload).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
                    && let Ok(lock) = open_lock(&entry.path())
                    && lock.try_lock().is_ok()
                {
                    drop(lock);
                    let _ = std::fs::remove_file(entry.path());
                }
                continue;
            }
            if !owned(&name) {
                continue;
            }
            let path = entry.path();
            let bytes = entry.metadata()?.len();
            let Ok(mut lock) = existing_lock(&lease_path(&path)) else {
                stats.remaining_bytes = stats.remaining_bytes.saturating_add(bytes);
                continue;
            };
            if let Err(e) = lock.try_lock() {
                stats.remaining_bytes = stats.remaining_bytes.saturating_add(bytes);
                // Only contention means someone holds it; a file system that can't lock says
                // nothing about use, and such an entry is left alone all the same.
                if matches!(e, TryLockError::WouldBlock) {
                    stats.pinned += 1;
                    stats.pinned_bytes = stats.pinned_bytes.saturating_add(bytes);
                }
                continue;
            }
            let flag = state(&mut lock).ok();
            if !matches!(flag, Some(b'C' | b'B' | b'T')) {
                stats.remaining_bytes = stats.remaining_bytes.saturating_add(bytes);
                continue;
            }
            let temporary = flag != Some(b'C') || !name.starts_with("rg-index-v1-");
            let modified = lock.metadata()?.modified().ok();
            let used = modified.unwrap_or(UNIX_EPOCH);
            // An index of an earlier version of a file just indexed again, or one unused for
            // longer than the limit (its file deleted or moved, say), is stale (#1230). An
            // unknown or future time never counts as unused.
            let superseded = replaced.is_some_and(|prefix| name.starts_with(prefix) && Some(name.as_str()) != newest);
            let idle = modified.and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > max_idle);
            // With no budget every entry not in use goes now: evicting from the candidates would
            // scan the whole folder again for every batch of them.
            if (temporary || superseded || idle || budget == 0) && std::fs::remove_file(&path).is_ok() {
                drop(lock);
                let _ = std::fs::remove_file(lease_path(&path));
                stats.removed += 1;
                stats.removed_bytes = stats.removed_bytes.saturating_add(bytes);
                continue;
            }
            stats.remaining_bytes = stats.remaining_bytes.saturating_add(bytes);
            oldest.push((used, name, bytes));
            if oldest.len() > CANDIDATES {
                oldest.pop();
            }
        }
        // Removing while listing a folder can make some file systems skip entries, and one skipped
        // would go unreported: scan again until a pass removes nothing.
        if budget == 0 && stats.removed > removed_before {
            continue;
        }
        if stats.remaining_bytes <= budget {
            break;
        }
        let mut removed = false;
        for (_, name, bytes) in oldest.into_sorted_vec() {
            if stats.remaining_bytes <= budget {
                break;
            }
            if remove_unpinned(&root.join(name)).unwrap_or(false) {
                stats.remaining_bytes = stats.remaining_bytes.saturating_sub(bytes);
                stats.removed += 1;
                stats.removed_bytes = stats.removed_bytes.saturating_add(bytes);
                removed = true;
            }
        }
        if !removed || stats.remaining_bytes <= budget {
            break;
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests;
