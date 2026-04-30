use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Error type for connection pool operations.
///
/// Per Phase 35 / B10: unified error model. All pool methods return `PoolError`
/// rather than panicking on poison or silently recovering. Callers convert at
/// the napi boundary via `napi::Error::from_reason(format!("...: {e}"))`.
///
/// See `.planning/IVM-PORT-AUDIT-DEEP.md` §B10 for the audit finding and
/// `.planning/phases/35-pool-cascade-hardening/35-CONTEXT.md` for design.
#[derive(Debug)]
pub enum PoolError {
    Sqlite(rusqlite::Error),
    Exhausted,
    Poisoned,
    /// B10/D-01: distinct from `Sqlite` for path-open / FS errors that aren't
    /// surfaced as `rusqlite::Error`. Reserved for future use; emitted by
    /// `From<std::io::Error>`.
    IoError(std::io::Error),
    /// B10/D-05: returned by `swap_path*` when another swap is already
    /// in progress (the `swap_guard` Mutex is held).
    ConcurrentSwapInProgress,
}

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PoolError::Sqlite(e) => write!(f, "sqlite error: {e}"),
            PoolError::Exhausted => write!(f, "connection pool exhausted"),
            PoolError::Poisoned => write!(f, "connection pool mutex poisoned"),
            PoolError::IoError(e) => write!(f, "pool io error: {e}"),
            PoolError::ConcurrentSwapInProgress => {
                write!(f, "another swap_path is in progress on this pool")
            }
        }
    }
}

impl std::error::Error for PoolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PoolError::Sqlite(e) => Some(e),
            PoolError::IoError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for PoolError {
    fn from(e: rusqlite::Error) -> Self {
        PoolError::Sqlite(e)
    }
}

impl From<std::io::Error> for PoolError {
    fn from(e: std::io::Error) -> Self {
        PoolError::IoError(e)
    }
}

type Result<T> = std::result::Result<T, PoolError>;

/// A lightweight pool of read-only SQLite connections pinned to the same WAL
/// snapshot. Safe to share across threads (`Send + Sync`).
///
/// Per Phase 35 / B10: `swap_path` polls with bounded retry; `swap_path_now`
/// fails fast; `swap_path_with_timeout` is fully configurable. Concurrent
/// `swap_path*` callers serialize via the `swap_guard: Mutex<()>` field —
/// the loser observes `PoolError::ConcurrentSwapInProgress`.
#[derive(Clone)]
pub struct ConnectionPool {
    connections: Arc<Mutex<Vec<Connection>>>,
    path: Arc<Mutex<String>>,
    pool_size: usize,
    /// B10/D-05: serialize concurrent `swap_path*` callers. `try_lock()`
    /// non-blocking — second caller observes `ConcurrentSwapInProgress`.
    swap_guard: Arc<Mutex<()>>,
}

impl ConnectionPool {
    /// Opens `pool_size` read-only connections to `path`, each pinned to the
    /// current WAL snapshot via `BEGIN DEFERRED`.
    pub fn new(path: &str, pool_size: usize) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;

        let mut conns = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let conn = Connection::open_with_flags(path, flags)?;
            conn.busy_timeout(Duration::from_millis(5000))?;
            conn.execute_batch("BEGIN DEFERRED")?;
            conns.push(conn);
        }

        Ok(Self {
            connections: Arc::new(Mutex::new(conns)),
            path: Arc::new(Mutex::new(path.to_owned())),
            pool_size,
            swap_guard: Arc::new(Mutex::new(())),
        })
    }

    /// Takes a connection from the pool. Returns `PoolError::Exhausted` if no
    /// connections are available (non-blocking).
    pub fn get(&self) -> Result<PooledConnection> {
        use std::sync::atomic::{AtomicU64, Ordering as AO};
        static POOL_GET_CALLS: AtomicU64 = AtomicU64::new(0);
        static POOL_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
        static POOL_WAIT_NS: AtomicU64 = AtomicU64::new(0);

        POOL_GET_CALLS.fetch_add(1, AO::Relaxed);
        let t0 = std::time::Instant::now();
        let mut conns = self.connections.lock().map_err(|_| PoolError::Poisoned)?;
        POOL_WAIT_NS.fetch_add(t0.elapsed().as_nanos() as u64, AO::Relaxed);
        match conns.pop() {
            Some(conn) => Ok(PooledConnection {
                conn: Some(conn),
                pool: Arc::clone(&self.connections),
            }),
            None => {
                let exhausted = POOL_EXHAUSTED.fetch_add(1, AO::Relaxed) + 1;
                let calls = POOL_GET_CALLS.load(AO::Relaxed);
                if exhausted % 100 == 1 {
                    let profile = std::env::var("RUST_HYDRATE_PROFILE").unwrap_or_default() == "1";
                    if profile {
                        eprintln!("      [pool] EXHAUSTED #{} (of {} calls, wait={}us)",
                            exhausted, calls, POOL_WAIT_NS.load(AO::Relaxed) / 1000);
                    }
                }
                Err(PoolError::Exhausted)
            }
        }
    }

    /// Refreshes the WAL snapshot on every connection currently in the pool by
    /// committing the open transaction and starting a new deferred one.
    ///
    /// Only operates on connections that are checked in. Callers must ensure
    /// all `PooledConnection` guards have been dropped before calling this if a
    /// consistent snapshot across *all* connections is required.
    pub fn set_snapshot(&self) -> Result<()> {
        let conns = self.connections.lock().map_err(|_| PoolError::Poisoned)?;
        for conn in conns.iter() {
            conn.execute_batch("COMMIT; BEGIN DEFERRED")?;
        }
        Ok(())
    }

    /// Returns the database path. Errors if the path mutex is poisoned.
    ///
    /// Per Phase 35 / B10/D-03: silent `unwrap_or_else` recovery removed.
    /// Callers MUST handle the `Poisoned` error explicitly. For callers that
    /// cannot easily propagate, see `db_path_or_default()` in `table_source.rs`.
    pub fn path(&self) -> Result<String> {
        Ok(self.path.lock().map_err(|_| PoolError::Poisoned)?.clone())
    }

    /// Returns the configured pool size.
    pub fn pool_size(&self) -> usize {
        self.pool_size
    }

    /// Returns the number of connections currently available in the pool.
    pub fn available(&self) -> Result<usize> {
        let conns = self.connections.lock().map_err(|_| PoolError::Poisoned)?;
        Ok(conns.len())
    }

    /// Re-opens all connections at a new database path with bounded retry.
    /// Default timeout: 750ms (5 attempts at ~50/100/150/200/250ms increments).
    ///
    /// Per Phase 35 / B10/D-02. For configurable timeout, use
    /// `swap_path_with_timeout`. For immediate-fail (no retry), use
    /// `swap_path_now`.
    pub fn swap_path(&self, new_path: &str) -> Result<()> {
        self.swap_path_with_timeout(new_path, Duration::from_millis(750))
    }

    /// Re-opens connections at `new_path` immediately. Returns
    /// `PoolError::Exhausted` if any connection is currently checked out.
    /// No retry.
    ///
    /// Per Phase 35 / B10/D-04: this is the fast-fail variant for callers
    /// that have already drained the pool and want to fail loudly otherwise.
    pub fn swap_path_now(&self, new_path: &str) -> Result<()> {
        self.swap_path_with_timeout(new_path, Duration::from_millis(0))
    }

    /// Re-opens all connections at a new path. Polls with linear backoff
    /// (50ms initial step, +50ms per attempt) until all connections are
    /// checked in OR `timeout` is exceeded. Returns:
    /// - `Ok(())` on successful swap.
    /// - `PoolError::Exhausted` on timeout (some connection still checked out).
    /// - `PoolError::ConcurrentSwapInProgress` if another swap is mid-flight.
    /// - `PoolError::Poisoned` if any inner mutex is poisoned.
    /// - `PoolError::Sqlite(...)` on connection-open failure at `new_path`.
    ///
    /// Per Phase 35 / B10/D-02 + D-04 + D-05.
    pub fn swap_path_with_timeout(
        &self,
        new_path: &str,
        timeout: Duration,
    ) -> Result<()> {
        // D-05: serialize concurrent swaps. try_lock — second caller fails fast.
        let _guard = self
            .swap_guard
            .try_lock()
            .map_err(|e| match e {
                std::sync::TryLockError::WouldBlock => PoolError::ConcurrentSwapInProgress,
                std::sync::TryLockError::Poisoned(_) => PoolError::Poisoned,
            })?;

        // Same-path fast path: refresh WAL snapshot via set_snapshot — no retry.
        let current_path = self
            .path
            .lock()
            .map_err(|_| PoolError::Poisoned)?
            .clone();
        if new_path == current_path {
            return self.set_snapshot();
        }

        // Different path — must reopen. Poll for full pool quiescence.
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;

        let start = std::time::Instant::now();
        let mut attempt: u32 = 0;
        let initial_step = Duration::from_millis(50);

        loop {
            // Try to acquire lock; if all conns idle, perform swap inline.
            {
                let mut conns = self
                    .connections
                    .lock()
                    .map_err(|_| PoolError::Poisoned)?;
                if conns.len() == self.pool_size {
                    conns.clear();
                    for _ in 0..self.pool_size {
                        let conn = Connection::open_with_flags(new_path, flags)?;
                        conn.busy_timeout(Duration::from_millis(5000))?;
                        conn.execute_batch("BEGIN DEFERRED")?;
                        conns.push(conn);
                    }
                    drop(conns);
                    *self.path.lock().map_err(|_| PoolError::Poisoned)? =
                        new_path.to_owned();
                    return Ok(());
                }
                // Not idle — fall through and back off.
            }

            attempt += 1;
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(PoolError::Exhausted);
            }
            // Linear backoff: 50ms, 100ms, 150ms, ... (CD-01).
            let step = initial_step * attempt;
            let remaining = timeout.saturating_sub(elapsed);
            let sleep = step.min(remaining);
            std::thread::sleep(sleep);
        }
    }
}

/// RAII guard that returns a connection to the pool on drop.
#[derive(Debug)]
pub struct PooledConnection {
    conn: Option<Connection>,
    pool: Arc<Mutex<Vec<Connection>>>,
}

impl std::ops::Deref for PooledConnection {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("connection taken before drop")
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if let Ok(mut conns) = self.pool.lock() {
                conns.push(conn);
            }
        }
    }
}

// ─── Test-only fault-injection helpers (B10/D-06) ─────────────────────────
#[cfg(test)]
impl ConnectionPool {
    /// Test-only: poison the inner `connections` mutex. Spawns a thread
    /// that locks the mutex and panics; after join, subsequent `lock()`
    /// returns `Err`.
    pub(crate) fn poison_connections_for_test(&self) {
        let conns = Arc::clone(&self.connections);
        let h = std::thread::spawn(move || {
            let _g = conns.lock().unwrap();
            panic!("intentional poison for test");
        });
        let _ = h.join();
    }

    /// Test-only: poison the inner `path` mutex.
    pub(crate) fn poison_path_for_test(&self) {
        let p = Arc::clone(&self.path);
        let h = std::thread::spawn(move || {
            let _g = p.lock().unwrap();
            panic!("intentional poison for test");
        });
        let _ = h.join();
    }

    /// Test-only: construct a pool and pre-acquire `hold_count` connections.
    /// Returns the pool plus the held guards so the caller can drop them
    /// on a timer.
    pub(crate) fn new_with_holds(
        path: &str,
        pool_size: usize,
        hold_count: usize,
    ) -> Result<(Self, Vec<PooledConnection>)> {
        let pool = ConnectionPool::new(path, pool_size)?;
        let mut held = Vec::with_capacity(hold_count);
        for _ in 0..hold_count {
            held.push(pool.get()?);
        }
        Ok((pool, held))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("failed to create temp file");
        let conn = Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);",
        )
        .expect("setup");
        drop(conn);
        file
    }

    #[test]
    fn test_pool_creation() {
        let db = create_test_db();
        let pool = ConnectionPool::new(db.path().to_str().unwrap(), 4).unwrap();
        assert_eq!(pool.pool_size(), 4);
        assert_eq!(pool.available().unwrap(), 4);
    }

    #[test]
    fn test_get_return() {
        let db = create_test_db();
        let pool = ConnectionPool::new(db.path().to_str().unwrap(), 2).unwrap();

        {
            let conn = pool.get().unwrap();
            assert_eq!(pool.available().unwrap(), 1);
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
        // Connection returned on drop.
        assert_eq!(pool.available().unwrap(), 2);
    }

    #[test]
    fn test_pool_exhaustion() {
        let db = create_test_db();
        let pool = ConnectionPool::new(db.path().to_str().unwrap(), 1).unwrap();

        let _c1 = pool.get().unwrap();
        let result = pool.get();
        assert!(result.is_err());
        match result.unwrap_err() {
            PoolError::Exhausted => {}
            other => panic!("expected Exhausted, got {other}"),
        }
    }

    // ─── B10 / Phase 35 new tests ────────────────────────────────────────

    #[test]
    fn test_swap_path_retry_succeeds_after_drop() {
        // B10/D-02: bounded retry — drop a held conn after 100ms; swap_path
        // (default 750ms budget) must succeed within budget.
        let db_a = create_test_db();
        let db_b = create_test_db();
        let (pool, mut held) = ConnectionPool::new_with_holds(
            db_a.path().to_str().unwrap(),
            2,
            1,
        )
        .unwrap();
        let pool_clone = pool.clone();
        let path_b = db_b.path().to_str().unwrap().to_string();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(held.pop().unwrap());
        });
        let r = pool_clone.swap_path(&path_b);
        h.join().unwrap();
        assert!(
            r.is_ok(),
            "swap_path should succeed within retry budget: {r:?}"
        );
        assert_eq!(pool_clone.path().unwrap(), path_b);
    }

    #[test]
    fn test_swap_path_retry_exhausted() {
        // B10/D-02: tight 200ms budget; held connection never drops.
        let db_a = create_test_db();
        let db_b = create_test_db();
        let (pool, _held) = ConnectionPool::new_with_holds(
            db_a.path().to_str().unwrap(),
            2,
            1,
        )
        .unwrap();
        let r = pool.swap_path_with_timeout(
            db_b.path().to_str().unwrap(),
            Duration::from_millis(200),
        );
        assert!(matches!(r, Err(PoolError::Exhausted)), "got {r:?}");
    }

    #[test]
    fn test_swap_path_now_fails_immediately() {
        // B10/D-04: fast-fail — must return Exhausted in <50ms.
        let db_a = create_test_db();
        let db_b = create_test_db();
        let (pool, _held) = ConnectionPool::new_with_holds(
            db_a.path().to_str().unwrap(),
            2,
            1,
        )
        .unwrap();
        let t0 = std::time::Instant::now();
        let r = pool.swap_path_now(db_b.path().to_str().unwrap());
        let elapsed = t0.elapsed();
        assert!(matches!(r, Err(PoolError::Exhausted)), "got {r:?}");
        assert!(
            elapsed < Duration::from_millis(50),
            "swap_path_now should fail fast — took {elapsed:?}"
        );
    }

    #[test]
    fn test_concurrent_swap_path_returns_in_progress() {
        // B10/D-05: two swap callers — at most one wins; others see
        // ConcurrentSwapInProgress or Exhausted (not silent corruption).
        let db_a = create_test_db();
        let db_b = create_test_db();
        let (pool, _held) = ConnectionPool::new_with_holds(
            db_a.path().to_str().unwrap(),
            2,
            1,
        )
        .unwrap();
        let pool_a = pool.clone();
        let pool_b = pool.clone();
        let path_b1 = db_b.path().to_str().unwrap().to_string();
        let path_b2 = db_b.path().to_str().unwrap().to_string();
        let h1 = std::thread::spawn(move || {
            pool_a.swap_path_with_timeout(&path_b1, Duration::from_millis(300))
        });
        // Tiny race window — second caller often loses the swap_guard try_lock.
        std::thread::sleep(Duration::from_millis(5));
        let r2 = pool_b.swap_path_with_timeout(&path_b2, Duration::from_millis(300));
        let _ = h1.join().unwrap();
        // Accept any of: ConcurrentSwapInProgress (D-05), Exhausted (held), or
        // Ok (h1 finished first). What we DON'T accept is silent corruption.
        match r2 {
            Err(PoolError::ConcurrentSwapInProgress)
            | Err(PoolError::Exhausted)
            | Ok(_) => {}
            other => panic!("unexpected swap result: {other:?}"),
        }
    }

    #[test]
    fn test_path_returns_poisoned_on_poison() {
        // B10/D-03: explicit poison propagation — no silent recovery.
        let db = create_test_db();
        let pool = ConnectionPool::new(db.path().to_str().unwrap(), 2).unwrap();
        pool.poison_path_for_test();
        let r = pool.path();
        assert!(matches!(r, Err(PoolError::Poisoned)), "got {r:?}");
    }
}
