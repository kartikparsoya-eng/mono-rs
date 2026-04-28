use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Error type for connection pool operations.
#[derive(Debug)]
pub enum PoolError {
    Sqlite(rusqlite::Error),
    Exhausted,
    Poisoned,
}

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PoolError::Sqlite(e) => write!(f, "sqlite error: {e}"),
            PoolError::Exhausted => write!(f, "connection pool exhausted"),
            PoolError::Poisoned => write!(f, "connection pool mutex poisoned"),
        }
    }
}

impl std::error::Error for PoolError {}

impl From<rusqlite::Error> for PoolError {
    fn from(e: rusqlite::Error) -> Self {
        PoolError::Sqlite(e)
    }
}

type Result<T> = std::result::Result<T, PoolError>;

/// A lightweight pool of read-only SQLite connections pinned to the same WAL
/// snapshot. Safe to share across threads (`Send + Sync`).
#[derive(Clone)]
pub struct ConnectionPool {
    connections: Arc<Mutex<Vec<Connection>>>,
    path: Arc<Mutex<String>>,
    pool_size: usize,
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

    /// Returns the database path.
    pub fn path(&self) -> String {
        self.path.lock().unwrap_or_else(|e| e.into_inner()).clone()
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

    /// Re-opens all connections at a new database path. Each connection gets
    /// a fresh `BEGIN DEFERRED` snapshot. All `PooledConnection` guards must
    /// be dropped before calling this — checked via available count.
    pub fn swap_path(&self, new_path: &str) -> Result<()> {
        let current_path = self.path.lock().map_err(|_| PoolError::Poisoned)?.clone();

        if new_path == current_path {
            // Same DB file (WAL mode) — just refresh the read snapshot
            // by ending the current transaction and starting a new one.
            // This is the fast path: no Connection::open syscalls.
            return self.set_snapshot();
        }

        // Different path — must reopen all connections.
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;

        let mut conns = self.connections.lock().map_err(|_| PoolError::Poisoned)?;
        if conns.len() != self.pool_size {
            return Err(PoolError::Exhausted);
        }
        conns.clear();
        for _ in 0..self.pool_size {
            let conn = Connection::open_with_flags(new_path, flags)?;
            conn.busy_timeout(Duration::from_millis(5000))?;
            conn.execute_batch("BEGIN DEFERRED")?;
            conns.push(conn);
        }
        drop(conns);

        *self.path.lock().map_err(|_| PoolError::Poisoned)? = new_path.to_owned();
        Ok(())
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
}
