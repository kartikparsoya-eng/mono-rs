/**
 * A/B Benchmark: zqlite (old TS/better-sqlite3) vs zqlite-rs (new Rust/napi)
 *
 * Compares the two native backends directly, using the same operations
 * that the zqlite wrapper's Database/Statement classes perform.
 *
 * "zqlite" = better-sqlite3 (the C++ addon the old db.ts used)
 * "zqlite-rs" = Rust napi-rs module (the new db.ts backend)
 */

const path = require('node:path');
const os = require('node:os');
const fs = require('node:fs');

// ── Load both backends ──
// Old: better-sqlite3 (what @rocicorp/zero-sqlite3 wraps)
const BetterSqlite3 = require('@rocicorp/zero-sqlite3');
// New: Rust zqlite-rs
const { Database: RustDatabase } = require(path.resolve(__dirname, '../index.js'));

const WARMUP = 500;
const ITERATIONS = 10000;
const BATCH_SIZE = 100;

function hrToMs(hr) {
  return hr[0] * 1e3 + hr[1] / 1e6;
}

function bench(name, fn, iterations = ITERATIONS) {
  for (let i = 0; i < WARMUP; i++) fn(i);

  const times = [];
  const start = process.hrtime();
  for (let i = 0; i < iterations; i++) {
    const t0 = process.hrtime();
    fn(i);
    const t1 = process.hrtime(t0);
    times.push(hrToMs(t1));
  }
  const total = process.hrtime(start);
  const totalMs = hrToMs(total);
  times.sort((a, b) => a - b);

  return {
    name,
    iterations,
    totalMs: totalMs.toFixed(2),
    opsPerSec: Math.round(iterations / (totalMs / 1000)),
    medianUs: (times[Math.floor(times.length / 2)] * 1000).toFixed(2),
    p99Us: (times[Math.floor(times.length * 0.99)] * 1000).toFixed(2),
  };
}

function tmpDb(label) {
  return path.join(os.tmpdir(), `bench-${label}-${Date.now()}.db`);
}

// ── Helpers to setup identical DBs for both backends ──
function setupOld(p) {
  const db = new BetterSqlite3(p);
  db.pragma('journal_mode=WAL');
  db.exec(`CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT, value REAL, data BLOB)`);
  db.exec(`CREATE TABLE kv (key TEXT PRIMARY KEY, val TEXT)`);
  return db;
}

function setupNew(p) {
  const db = new RustDatabase(p);
  db.exec('PRAGMA journal_mode=WAL');
  db.exec(`CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT, value REAL, data BLOB)`);
  db.exec(`CREATE TABLE kv (key TEXT PRIMARY KEY, val TEXT)`);
  return db;
}

function seedOld(db, n) {
  const stmt = db.prepare('INSERT INTO bench (name, value, data) VALUES (?, ?, ?)');
  const tx = db.transaction(() => {
    for (let i = 0; i < n; i++) stmt.run(`name-${i}`, Math.random() * 1000, Buffer.from(`data-${i}`));
  });
  tx();
}

function seedNew(db, n) {
  const stmt = db.prepare('INSERT INTO bench (name, value, data) VALUES (?, ?, ?)');
  db.exec('BEGIN');
  for (let i = 0; i < n; i++) stmt.run([`name-${i}`, Math.random() * 1000, Buffer.from(`data-${i}`)]);
  db.exec('COMMIT');
}

// ══════════════════════════════════════════════════════════════
console.log('='.repeat(72));
console.log('  A/B Benchmark: zqlite (better-sqlite3) vs zqlite-rs (Rust)');
console.log(`  Node ${process.version} | ${os.platform()}-${os.arch()}`);
console.log(`  Iterations: ${ITERATIONS} | Warmup: ${WARMUP}`);
console.log('='.repeat(72));

const results = [];

// ── 1. INSERT single row ──
{
  const p1 = tmpDb('old-ins'), p2 = tmpDb('new-ins');
  const old = setupOld(p1), neo = setupNew(p2);
  const s1 = old.prepare('INSERT INTO kv (key, val) VALUES (?, ?)');
  const s2 = neo.prepare('INSERT INTO kv (key, val) VALUES (?, ?)');
  let c1 = 0, c2 = 0;

  results.push(bench('INSERT single  [zqlite]   ', () => { s1.run(`k${c1++}`, 'v'); }));
  results.push(bench('INSERT single  [zqlite-rs]', () => { s2.run([`k${c2++}`, 'v']); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 2. SELECT get (single row by PK) ──
{
  const p1 = tmpDb('old-get'), p2 = tmpDb('new-get');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 1000); seedNew(neo, 1000);
  const s1 = old.prepare('SELECT * FROM bench WHERE id = ?');
  const s2 = neo.prepare('SELECT * FROM bench WHERE id = ?');

  results.push(bench('SELECT get     [zqlite]   ', (i) => { s1.get((i % 1000) + 1); }));
  results.push(bench('SELECT get     [zqlite-rs]', (i) => { s2.get([(i % 1000) + 1]); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 3. SELECT all (100 rows) ──
{
  const p1 = tmpDb('old-all'), p2 = tmpDb('new-all');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 100); seedNew(neo, 100);
  const s1 = old.prepare('SELECT * FROM bench');
  const s2 = neo.prepare('SELECT * FROM bench');

  results.push(bench('SELECT all(100)[zqlite]   ', () => { s1.all(); }));
  results.push(bench('SELECT all(100)[zqlite-rs]', () => { s2.all([]); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 4. Iterate 100 rows ──
{
  const p1 = tmpDb('old-iter'), p2 = tmpDb('new-iter');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 100); seedNew(neo, 100);
  const s1 = old.prepare('SELECT * FROM bench');
  const s2 = neo.prepare('SELECT * FROM bench');

  results.push(bench('Iterate(100)   [zqlite]   ', () => {
    for (const _ of s1.iterate()) {}
  }));
  results.push(bench('Iterate(100)   [zqlite-rs]', () => {
    const it = s2.iterate([]);
    let r;
    while (!(r = it.next()).done) {}
  }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 5. Transaction batch (100 inserts per txn) ──
{
  const p1 = tmpDb('old-txn'), p2 = tmpDb('new-txn');
  const old = setupOld(p1), neo = setupNew(p2);
  const s1 = old.prepare('INSERT INTO kv (key, val) VALUES (?, ?)');
  const s2 = neo.prepare('INSERT INTO kv (key, val) VALUES (?, ?)');

  const oldTxn = old.transaction((base) => {
    for (let j = 0; j < BATCH_SIZE; j++) s1.run(`k${base + j}`, `v${j}`);
  });

  let c1 = 0, c2 = 0;
  results.push(bench('Txn batch(100) [zqlite]   ', () => { oldTxn(c1); c1 += BATCH_SIZE; }, 2000));
  results.push(bench('Txn batch(100) [zqlite-rs]', () => {
    neo.exec('BEGIN');
    for (let j = 0; j < BATCH_SIZE; j++) s2.run([`k${c2 + j}`, `v${j}`]);
    neo.exec('COMMIT');
    c2 += BATCH_SIZE;
  }, 2000));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 6. Large result set (1000 rows) ──
{
  const p1 = tmpDb('old-1k'), p2 = tmpDb('new-1k');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 1000); seedNew(neo, 1000);
  const s1 = old.prepare('SELECT * FROM bench');
  const s2 = neo.prepare('SELECT * FROM bench');

  results.push(bench('SELECT all(1K) [zqlite]   ', () => { s1.all(); }, 2000));
  results.push(bench('SELECT all(1K) [zqlite-rs]', () => { s2.all([]); }, 2000));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── Print results ──
console.log('');
console.log('+--------------------------------------+----------+------------+-----------+----------+');
console.log('| Test                                 | ops/sec  | total(ms)  | med(us)   | p99(us)  |');
console.log('+--------------------------------------+----------+------------+-----------+----------+');
for (const r of results) {
  const n = r.name.padEnd(36);
  const ops = String(r.opsPerSec).padStart(8);
  const tot = String(r.totalMs).padStart(10);
  const med = String(r.medianUs).padStart(9);
  const p99 = String(r.p99Us).padStart(8);
  console.log(`| ${n} | ${ops} | ${tot} | ${med} | ${p99} |`);
}
console.log('+--------------------------------------+----------+------------+-----------+----------+');

// ── Speedup summary ──
console.log('\n--- Speedup: zqlite-rs / zqlite ---');
for (let i = 0; i < results.length; i += 2) {
  const old = results[i];
  const neo = results[i + 1];
  const ratio = (neo.opsPerSec / old.opsPerSec).toFixed(2);
  const label = old.name.replace('[zqlite]', '').trim();
  const arrow = ratio >= 1 ? 'FASTER' : 'slower';
  console.log(`  ${label}: ${ratio}x (${arrow}) -- ${neo.opsPerSec} vs ${old.opsPerSec} ops/sec`);
}
