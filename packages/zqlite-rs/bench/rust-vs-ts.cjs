/**
 * A/B Benchmark: zqlite (better-sqlite3) vs zqlite-rs (Rust/napi-rs)
 *
 * Tests both the generic Statement API and the optimized Database-level
 * methods (queryAll, getRow, getRowsBuf, changesSinceBuf, allBuf).
 *
 * "zqlite"    = better-sqlite3 (C++ addon)
 * "zqlite-rs" = Rust napi-rs module
 */

const path = require('node:path');
const os = require('node:os');
const fs = require('node:fs');

// ── Load both backends ──
const BetterSqlite3 = require('@rocicorp/zero-sqlite3');
const { Database: RustDatabase } = require(path.resolve(__dirname, '../index.js'));

// decodeBuf from db.ts (inline for benchmark)
function decodeBuf(buf, colNames) {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  let offset = 0;
  const rowCount = dv.getUint32(offset, true); offset += 4;
  const colCount = dv.getUint16(offset, true); offset += 2;
  const rows = new Array(rowCount);
  const td = new TextDecoder();
  for (let r = 0; r < rowCount; r++) {
    const row = {};
    for (let c = 0; c < colCount; c++) {
      const tag = dv.getUint8(offset); offset += 1;
      switch (tag) {
        case 0: row[colNames[c]] = null; break;
        case 1: {
          const lo = dv.getUint32(offset, true);
          const hi = dv.getInt32(offset + 4, true);
          const val = hi * 0x100000000 + (lo >>> 0);
          row[colNames[c]] = val;
          offset += 8;
          break;
        }
        case 2:
          row[colNames[c]] = dv.getFloat64(offset, true);
          offset += 8;
          break;
        case 3: {
          const len = dv.getUint32(offset, true); offset += 4;
          row[colNames[c]] = td.decode(new Uint8Array(buf.buffer, buf.byteOffset + offset, len));
          offset += len;
          break;
        }
        case 4: {
          const len = dv.getUint32(offset, true); offset += 4;
          row[colNames[c]] = buf.slice(offset, offset + len);
          offset += len;
          break;
        }
      }
    }
    rows[r] = row;
  }
  return rows;
}

const WARMUP = 500;
const ITERATIONS = 10000;
const BATCH_SIZE = 100;

function hrToUs(hr) {
  return hr[0] * 1e6 + hr[1] / 1e3;
}

function bench(name, fn, iterations = ITERATIONS) {
  // Warmup
  for (let i = 0; i < WARMUP; i++) fn(i);

  const times = new Float64Array(iterations);
  const start = process.hrtime();
  for (let i = 0; i < iterations; i++) {
    const t0 = process.hrtime();
    fn(i);
    const t1 = process.hrtime(t0);
    times[i] = hrToUs(t1);
  }
  const total = process.hrtime(start);
  const totalMs = total[0] * 1e3 + total[1] / 1e6;
  times.sort();

  return {
    name,
    iterations,
    totalMs: totalMs.toFixed(2),
    opsPerSec: Math.round(iterations / (totalMs / 1000)),
    medianUs: times[Math.floor(times.length / 2)].toFixed(2),
    p99Us: times[Math.floor(times.length * 0.99)].toFixed(2),
  };
}

function tmpDb(label) {
  return path.join(os.tmpdir(), `bench-${label}-${Date.now()}-${Math.random().toString(36).slice(2)}.db`);
}

// ── Setup helpers ──
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
console.log('='.repeat(80));
console.log('  A/B Benchmark: zqlite (better-sqlite3) vs zqlite-rs (Rust)');
console.log(`  Node ${process.version} | ${os.platform()}-${os.arch()}`);
console.log(`  Iterations: ${ITERATIONS} | Warmup: ${WARMUP}`);
console.log('='.repeat(80));

const results = [];
const sections = [];

function section(name) {
  sections.push({ name, startIdx: results.length });
}

// ═══════════════════════════════════════════════════════════════
// SECTION 1: Statement API (generic paths)
// ═══════════════════════════════════════════════════════════════
section('Statement API (generic)');

// ── 1a. INSERT single row ──
{
  const p1 = tmpDb('old-ins'), p2 = tmpDb('new-ins');
  const old = setupOld(p1), neo = setupNew(p2);
  const s1 = old.prepare('INSERT INTO kv (key, val) VALUES (?, ?)');
  const s2 = neo.prepare('INSERT INTO kv (key, val) VALUES (?, ?)');
  let c1 = 0, c2 = 0;

  results.push(bench('INSERT single  [sqlite3]', () => { s1.run(`k${c1++}`, 'v'); }));
  results.push(bench('INSERT single  [rust]   ', () => { s2.run([`k${c2++}`, 'v']); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 1b. INSERT with named params ──
{
  const p1 = tmpDb('old-named'), p2 = tmpDb('new-named');
  const old = setupOld(p1), neo = setupNew(p2);
  const s1 = old.prepare('INSERT INTO kv (key, val) VALUES (@key, @val)');
  const s2 = neo.prepare('INSERT INTO kv (key, val) VALUES (@key, @val)');
  let c1 = 0, c2 = 0;

  results.push(bench('INSERT named   [sqlite3]', () => { s1.run({key: `k${c1++}`, val: 'v'}); }));
  results.push(bench('INSERT named   [rust]   ', () => { s2.run([{key: `k${c2++}`, val: 'v'}]); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 2. SELECT get (single row by PK) ──
{
  const p1 = tmpDb('old-get'), p2 = tmpDb('new-get');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 1000); seedNew(neo, 1000);
  const s1 = old.prepare('SELECT * FROM bench WHERE id = ?');
  const s2 = neo.prepare('SELECT * FROM bench WHERE id = ?');

  results.push(bench('SELECT get     [sqlite3]', (i) => { s1.get((i % 1000) + 1); }));
  results.push(bench('SELECT get     [rust]   ', (i) => { s2.get([(i % 1000) + 1]); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 3. SELECT all (100 rows) ──
{
  const p1 = tmpDb('old-all'), p2 = tmpDb('new-all');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 100); seedNew(neo, 100);
  const s1 = old.prepare('SELECT * FROM bench');
  const s2 = neo.prepare('SELECT * FROM bench');

  results.push(bench('SELECT all(100)[sqlite3]', () => { s1.all(); }));
  results.push(bench('SELECT all(100)[rust]   ', () => { s2.all([]); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 4. SELECT all (1000 rows) ──
{
  const p1 = tmpDb('old-1k'), p2 = tmpDb('new-1k');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 1000); seedNew(neo, 1000);
  const s1 = old.prepare('SELECT * FROM bench');
  const s2 = neo.prepare('SELECT * FROM bench');

  results.push(bench('SELECT all(1K) [sqlite3]', () => { s1.all(); }, 2000));
  results.push(bench('SELECT all(1K) [rust]   ', () => { s2.all([]); }, 2000));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ═══════════════════════════════════════════════════════════════
// SECTION 2: allBuf protocol (hot path optimization)
// ═══════════════════════════════════════════════════════════════
section('allBuf protocol (binary transfer)');

// ── 5. allBuf(100) vs all(100) ──
{
  const p1 = tmpDb('old-buf100'), p2 = tmpDb('new-buf100');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 100); seedNew(neo, 100);
  const s1 = old.prepare('SELECT * FROM bench');
  const s2 = neo.prepare('SELECT * FROM bench');
  const colNames = ['id', 'name', 'value', 'data'];

  results.push(bench('all(100)       [sqlite3]', () => { s1.all(); }));
  results.push(bench('allBuf(100)    [rust]   ', () => { decodeBuf(s2.allBuf([]), colNames); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 6. allBuf(1K) vs all(1K) ──
{
  const p1 = tmpDb('old-buf1k'), p2 = tmpDb('new-buf1k');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 1000); seedNew(neo, 1000);
  const s1 = old.prepare('SELECT * FROM bench');
  const s2 = neo.prepare('SELECT * FROM bench');
  const colNames = ['id', 'name', 'value', 'data'];

  results.push(bench('all(1K)        [sqlite3]', () => { s1.all(); }, 2000));
  results.push(bench('allBuf(1K)     [rust]   ', () => { decodeBuf(s2.allBuf([]), colNames); }, 2000));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 7. allBuf(5K) vs all(5K) ──
{
  const p1 = tmpDb('old-buf5k'), p2 = tmpDb('new-buf5k');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 5000); seedNew(neo, 5000);
  const s1 = old.prepare('SELECT * FROM bench');
  const s2 = neo.prepare('SELECT * FROM bench');
  const colNames = ['id', 'name', 'value', 'data'];

  results.push(bench('all(5K)        [sqlite3]', () => { s1.all(); }, 500));
  results.push(bench('allBuf(5K)     [rust]   ', () => { decodeBuf(s2.allBuf([]), colNames); }, 500));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ═══════════════════════════════════════════════════════════════
// SECTION 3: Database-level optimized methods (snapshotter paths)
// ═══════════════════════════════════════════════════════════════
section('Database methods (snapshotter hot path)');

// ── 8. getRow (single PK lookup with type conversion) ──
{
  const p1 = tmpDb('old-getrow'), p2 = tmpDb('new-getrow');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 1000); seedNew(neo, 1000);
  const s1 = old.prepare('SELECT * FROM bench WHERE id = ?');
  const sql = 'SELECT * FROM bench WHERE id = ?';
  const columnTypes = { value: 'FLOAT8' };

  results.push(bench('getRow(PK)     [sqlite3]', (i) => { s1.get((i % 1000) + 1); }));
  results.push(bench('getRow(PK)     [rust]   ', (i) => { neo.getRow(sql, [(i % 1000) + 1], columnTypes, 'bench'); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 9. getRowsBuf (multi-key OR query, snapshotter pattern) ──
{
  const p1 = tmpDb('old-getrows'), p2 = tmpDb('new-getrows');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 1000); seedNew(neo, 1000);
  // Simulate fetching 10 rows by PK (typical snapshotter batch)
  const sql10 = 'SELECT * FROM bench WHERE id=? OR id=? OR id=? OR id=? OR id=? OR id=? OR id=? OR id=? OR id=? OR id=?';
  const s1 = old.prepare(sql10);
  const colNames = ['id', 'name', 'value', 'data'];
  const params10 = [1, 50, 100, 200, 300, 400, 500, 600, 700, 800];

  results.push(bench('getRows(10)    [sqlite3]', () => { s1.all(...params10); }));
  results.push(bench('getRowsBuf(10) [rust]   ', () => { decodeBuf(neo.getRowsBuf(sql10, params10), colNames); }));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 10. queryAll (table-source with type conversion) ──
{
  const p1 = tmpDb('old-qa'), p2 = tmpDb('new-qa');
  const old = setupOld(p1), neo = setupNew(p2);
  seedOld(old, 500); seedNew(neo, 500);
  const s1 = old.prepare('SELECT * FROM bench LIMIT 100');
  const sql = 'SELECT * FROM bench LIMIT 100';
  const columnTypes = { value: 'FLOAT8' };

  results.push(bench('queryAll(100)  [sqlite3]', () => { s1.all(); }, 5000));
  results.push(bench('queryAll(100)  [rust]   ', () => { neo.queryAll(sql, [], columnTypes, 'bench'); }, 5000));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ═══════════════════════════════════════════════════════════════
// SECTION 4: Realistic workloads
// ═══════════════════════════════════════════════════════════════
section('Realistic workloads');

// ── 11. changesSince pattern (read changelog + decode) ──
{
  const p1 = tmpDb('old-cl'), p2 = tmpDb('new-cl');
  const old = setupOld(p1), neo = setupNew(p2);
  // Create a changeLog-like table
  old.exec(`CREATE TABLE "_zero.changeLog2" (stateVersion TEXT, pos INTEGER, "table" TEXT, rowKey TEXT, op TEXT)`);
  neo.exec(`CREATE TABLE "_zero.changeLog2" (stateVersion TEXT, pos INTEGER, "table" TEXT, rowKey TEXT, op TEXT)`);
  // Seed 500 entries
  const ins1 = old.prepare('INSERT INTO "_zero.changeLog2" VALUES (?, ?, ?, ?, ?)');
  const tx1 = old.transaction(() => {
    for (let i = 0; i < 500; i++) ins1.run(`0${String(i).padStart(3, '0')}`, i, 'users', `{"id":${i}}`, 's');
  });
  tx1();
  const ins2 = neo.prepare('INSERT INTO "_zero.changeLog2" VALUES (?, ?, ?, ?, ?)');
  neo.exec('BEGIN');
  for (let i = 0; i < 500; i++) ins2.run([`0${String(i).padStart(3, '0')}`, i, 'users', `{"id":${i}}`, 's']);
  neo.exec('COMMIT');

  const clSql = 'SELECT stateVersion, "table", rowKey, op FROM "_zero.changeLog2" WHERE stateVersion > ? ORDER BY stateVersion, pos LIMIT ? OFFSET ?';
  const s1 = old.prepare(clSql);
  const colNames = ['stateVersion', 'table', 'rowKey', 'op'];

  results.push(bench('changeLog(500) [sqlite3]', () => { s1.all('0000', 500, 0); }, 2000));
  results.push(bench('changesSince   [rust]   ', () => { decodeBuf(neo.changesSinceBuf('0000', 500, 0), colNames); }, 2000));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ── 12. Mixed read pattern (simulates Diff iterator: 1 changesSince + N getRow) ──
{
  const p1 = tmpDb('old-mix'), p2 = tmpDb('new-mix');
  const old = setupOld(p1), neo = setupNew(p2);
  old.exec(`CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, handle TEXT, "_0_version" TEXT)`);
  neo.exec(`CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, handle TEXT, "_0_version" TEXT)`);
  old.exec(`CREATE TABLE "_zero.changeLog2" (stateVersion TEXT, pos INTEGER, "table" TEXT, rowKey TEXT, op TEXT)`);
  neo.exec(`CREATE TABLE "_zero.changeLog2" (stateVersion TEXT, pos INTEGER, "table" TEXT, rowKey TEXT, op TEXT)`);

  // Seed 100 users + 100 change log entries
  const insUser1 = old.prepare('INSERT INTO users VALUES (?, ?, ?, ?)');
  const insCl1 = old.prepare('INSERT INTO "_zero.changeLog2" VALUES (?, ?, ?, ?, ?)');
  const tx = old.transaction(() => {
    for (let i = 0; i < 100; i++) {
      insUser1.run(i, `user-${i}`, `handle-${i}`, '05');
      insCl1.run('05', i, 'users', `{"id":${i}}`, 's');
    }
  });
  tx();

  const insUser2 = neo.prepare('INSERT INTO users VALUES (?, ?, ?, ?)');
  const insCl2 = neo.prepare('INSERT INTO "_zero.changeLog2" VALUES (?, ?, ?, ?, ?)');
  neo.exec('BEGIN');
  for (let i = 0; i < 100; i++) {
    insUser2.run([i, `user-${i}`, `handle-${i}`, '05']);
    insCl2.run(['05', i, 'users', `{"id":${i}}`, 's']);
  }
  neo.exec('COMMIT');

  const clStmt = old.prepare('SELECT stateVersion, "table", rowKey, op FROM "_zero.changeLog2" WHERE stateVersion > ? ORDER BY stateVersion, pos LIMIT ? OFFSET ?');
  const getStmt = old.prepare('SELECT * FROM users WHERE id = ?');
  const getRowSql = 'SELECT * FROM users WHERE id = ?';
  const clColNames = ['stateVersion', 'table', 'rowKey', 'op'];
  const columnTypes = {};

  // Simulates: read changeLog, then for each change, getRow
  results.push(bench('diff(100)      [sqlite3]', () => {
    const changes = clStmt.all('00', 100, 0);
    for (const c of changes) {
      const key = JSON.parse(c.rowKey);
      getStmt.get(key.id);
    }
  }, 1000));
  results.push(bench('diff(100)      [rust]   ', () => {
    const changes = decodeBuf(neo.changesSinceBuf('00', 100, 0), clColNames);
    for (const c of changes) {
      const key = JSON.parse(c.rowKey);
      neo.getRow(getRowSql, [key.id], columnTypes, 'users');
    }
  }, 1000));

  // Also benchmark getRowsMultiBuf: batch all 100 getRow calls into one napi crossing
  const allIds = Array.from({length: 100}, (_, i) => i);
  const colNames = ['id', 'name', 'handle', '_0_version'];
  results.push(bench('diff(100)multi [rust]   ', () => {
    const changes = decodeBuf(neo.changesSinceBuf('00', 100, 0), clColNames);
    const ids = changes.map(c => JSON.parse(c.rowKey).id);
    decodeBuf(neo.getRowsMultiBuf(getRowSql, ids, 1), colNames);
  }, 1000));
  results.push(null); // placeholder to maintain pairing

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ═══════════════════════════════════════════════════════════════
// SECTION 5: Txn write (for completeness)
// ═══════════════════════════════════════════════════════════════
section('Transaction writes');

// ── 13. Transaction batch (100 inserts per txn) ──
{
  const p1 = tmpDb('old-txn'), p2 = tmpDb('new-txn');
  const old = setupOld(p1), neo = setupNew(p2);
  const s1 = old.prepare('INSERT INTO kv (key, val) VALUES (?, ?)');
  const s2 = neo.prepare('INSERT INTO kv (key, val) VALUES (?, ?)');

  const oldTxn = old.transaction((base) => {
    for (let j = 0; j < BATCH_SIZE; j++) s1.run(`k${base + j}`, `v${j}`);
  });

  let c1 = 0, c2 = 0;
  results.push(bench('Txn batch(100) [sqlite3]', () => { oldTxn(c1); c1 += BATCH_SIZE; }, 2000));
  results.push(bench('Txn batch(100) [rust]   ', () => {
    neo.exec('BEGIN');
    for (let j = 0; j < BATCH_SIZE; j++) s2.run([`k${c2 + j}`, `v${j}`]);
    neo.exec('COMMIT');
    c2 += BATCH_SIZE;
  }, 2000));

  old.close(); neo.close(); fs.unlinkSync(p1); fs.unlinkSync(p2);
}

// ═══════════════════════════════════════════════════════════════
// Print results
// ═══════════════════════════════════════════════════════════════
console.log('');

const SEP = '+' + '-'.repeat(40) + '+' + '-'.repeat(10) + '+' + '-'.repeat(12) + '+' + '-'.repeat(11) + '+' + '-'.repeat(10) + '+';
const HDR = '| Test                                   | ops/sec  | total(ms)  | med(us)   | p99(us)  |';

let sectionIdx = 0;
for (let i = 0; i < results.length; i++) {
  // Print section header
  if (sectionIdx < sections.length && sections[sectionIdx].startIdx === i) {
    console.log('');
    console.log(`  ── ${sections[sectionIdx].name} ──`);
    console.log(SEP);
    console.log(HDR);
    console.log(SEP);
    sectionIdx++;
  }

  const r = results[i];
  if (!r) { continue; }
  const n = r.name.padEnd(38);
  const ops = String(r.opsPerSec).padStart(8);
  const tot = String(r.totalMs).padStart(10);
  const med = String(r.medianUs).padStart(9);
  const p99 = String(r.p99Us).padStart(8);
  console.log(`| ${n} | ${ops} | ${tot} | ${med} | ${p99} |`);

  // Print separator between pairs
  if (i % 2 === 1) {
    const old = results[i - 1];
    const neo = results[i];
    const ratio = (neo.opsPerSec / old.opsPerSec).toFixed(2);
    const marker = ratio >= 1.0 ? `  ✓ ${ratio}x FASTER` : `  ✗ ${ratio}x`;
    console.log(`|${marker.padEnd(40)}|${' '.repeat(10)}|${' '.repeat(12)}|${' '.repeat(11)}|${' '.repeat(10)}|`);
    if (i < results.length - 1) console.log(SEP);
  }
}
console.log(SEP);

// ── Summary ──
console.log('\n' + '═'.repeat(80));
console.log('  SUMMARY: Rust/sqlite3 ratio (>1.0 = Rust faster)');
console.log('═'.repeat(80));
const wins = [], losses = [];
for (let i = 0; i < results.length; i += 2) {
  const old = results[i];
  const neo = results[i + 1];
  if (!old || !neo) continue;
  const ratio = neo.opsPerSec / old.opsPerSec;
  const label = old.name.replace('[sqlite3]', '').trim();
  const entry = { label, ratio: ratio.toFixed(2), oldOps: old.opsPerSec, newOps: neo.opsPerSec };
  if (ratio >= 1.0) wins.push(entry); else losses.push(entry);
}

if (wins.length > 0) {
  console.log('\n  Rust WINS:');
  for (const w of wins) {
    console.log(`    ${w.ratio}x  ${w.label} (${w.newOps} vs ${w.oldOps} ops/sec)`);
  }
}
if (losses.length > 0) {
  console.log('\n  Rust SLOWER:');
  for (const l of losses.sort((a, b) => b.ratio - a.ratio)) {
    console.log(`    ${l.ratio}x  ${l.label} (${l.newOps} vs ${l.oldOps} ops/sec)`);
  }
}
console.log('');

// ── Regression assertions (run with --assert to enforce) ──
if (process.argv.includes('--assert')) {
  const all = [...wins, ...losses];
  const find = (label) => all.find(e => e.label.includes(label));
  const assertions = [
    ['allBuf(1K)', 1.5, find('all(1K)')],
    ['allBuf(5K)', 1.5, find('all(5K)')],
    ['allBuf(100)', 1.3, find('all(100)')],
  ];
  let failed = 0;
  for (const [name, min, entry] of assertions) {
    if (!entry) { console.log(`  SKIP: ${name} not found`); continue; }
    const ratio = parseFloat(entry.ratio);
    if (ratio < min) {
      console.error(`  FAIL: ${name} ratio ${ratio} < minimum ${min}`);
      failed++;
    } else {
      console.log(`  PASS: ${name} ratio ${ratio} >= ${min}`);
    }
  }
  if (failed > 0) process.exit(1);
  console.log('\n  All regression assertions passed.');
}
