import {describe, expect, test} from 'vitest';
import {createSilentLogContext} from '../../shared/src/logging-test-utils.ts';
import {Database, decodeBuf} from './db.ts';

/**
 * Fixture tests verifying .allBuf() + decodeBuf() returns identical data
 * to .all() across all SQLite column types.
 */
describe('allBuf equivalence', () => {
  function makeDb() {
    return new Database(createSilentLogContext(), ':memory:');
  }

  test('integers (small, zero, negative)', () => {
    const db = makeDb();
    db.exec(
      'CREATE TABLE t (a INTEGER, b INTEGER, c INTEGER, d INTEGER, e INTEGER)',
    );
    db.exec('INSERT INTO t VALUES (0, 1, -1, 42, 2147483647)');
    db.exec('INSERT INTO t VALUES (-2147483648, 100, 255, 65535, 123456789)');

    const stmt = db.prepare('SELECT * FROM t');
    const allResult = stmt.all();
    const buf = stmt.allBuf();
    const bufResult = decodeBuf(buf, ['a', 'b', 'c', 'd', 'e']);

    expect(bufResult).toEqual(allResult);
  });

  test('large integers (BigInt boundary)', () => {
    const db = makeDb();
    db.exec('CREATE TABLE t (val INTEGER)');
    // MAX_SAFE_INTEGER = 9007199254740991
    db.exec('INSERT INTO t VALUES (9007199254740990)'); // safe
    db.exec('INSERT INTO t VALUES (9007199254740991)'); // boundary → BigInt
    db.exec('INSERT INTO t VALUES (9007199254740992)'); // unsafe → BigInt
    db.exec('INSERT INTO t VALUES (-9007199254740991)'); // boundary → BigInt

    const stmt = db.prepare('SELECT * FROM t');
    stmt.safeIntegers(true);
    const allResult = stmt.all();
    const bufRaw = stmt.allBuf();
    const bufResult = decodeBuf(bufRaw, ['val']);

    expect(bufResult).toEqual(allResult);
  });

  test('floats', () => {
    const db = makeDb();
    db.exec('CREATE TABLE t (a REAL, b REAL, c REAL)');
    db.exec('INSERT INTO t VALUES (3.14159, -0.001, 1.7976931348623157e+308)');
    db.exec('INSERT INTO t VALUES (0.0, -273.15, 2.2250738585072014e-308)');

    const stmt = db.prepare('SELECT * FROM t');
    const allResult = stmt.all();
    const buf = stmt.allBuf();
    const bufResult = decodeBuf(buf, ['a', 'b', 'c']);

    expect(bufResult).toEqual(allResult);
  });

  test('text (ASCII, unicode, empty)', () => {
    const db = makeDb();
    db.exec('CREATE TABLE t (val TEXT)');
    const stmt = db.prepare('INSERT INTO t VALUES (?)');
    const texts = [
      'hello',
      '',
      '日本語テスト',
      'émojis: 🎉🚀',
      'line\nnewline',
    ];
    for (const t of texts) {
      stmt.run(t);
    }

    const sel = db.prepare('SELECT * FROM t');
    const allResult = sel.all();
    const buf = sel.allBuf();
    const bufResult = decodeBuf(buf, ['val']);

    expect(bufResult).toEqual(allResult);
  });

  test('blobs', () => {
    const db = makeDb();
    db.exec('CREATE TABLE t (data BLOB)');
    const stmt = db.prepare('INSERT INTO t VALUES (?)');
    const blobs = [
      Buffer.from([0, 1, 2, 3, 255]),
      Buffer.from([]),
      Buffer.from('hello world'),
      Buffer.alloc(1024, 0xab),
    ];
    for (const b of blobs) {
      stmt.run(b);
    }

    const sel = db.prepare('SELECT * FROM t');
    const allResult = sel.all<{data: Buffer}>();
    const buf = sel.allBuf();
    const bufResult = decodeBuf<{data: Buffer}>(buf, ['data']);

    // Compare buffer contents (Buffer equality)
    expect(bufResult.length).toBe(allResult.length);
    for (let i = 0; i < allResult.length; i++) {
      expect(
        Buffer.from(bufResult[i].data).equals(Buffer.from(allResult[i].data)),
      ).toBe(true);
    }
  });

  test('nulls across all types', () => {
    const db = makeDb();
    db.exec('CREATE TABLE t (i INTEGER, r REAL, t TEXT, b BLOB)');
    db.exec('INSERT INTO t VALUES (NULL, NULL, NULL, NULL)');
    db.exec("INSERT INTO t VALUES (1, 2.5, 'hi', X'FF')");
    db.exec('INSERT INTO t VALUES (NULL, 3.0, NULL, NULL)');

    const stmt = db.prepare('SELECT * FROM t');
    const allResult = stmt.all();
    const buf = stmt.allBuf();
    const bufResult = decodeBuf(buf, ['i', 'r', 't', 'b']);

    // For rows with blobs, compare buffers specially
    expect(bufResult.length).toBe(allResult.length);
    expect(bufResult[0]).toEqual(allResult[0]); // all nulls
    // Row 2: has a blob
    expect(bufResult[1].i).toBe((allResult[1] as Record<string, unknown>).i);
    expect(bufResult[1].r).toBe((allResult[1] as Record<string, unknown>).r);
    expect(bufResult[1].t).toBe((allResult[1] as Record<string, unknown>).t);
    expect(
      Buffer.from(bufResult[1].b as Buffer).equals(
        Buffer.from((allResult[1] as Record<string, unknown>).b as Buffer),
      ),
    ).toBe(true);
    expect(bufResult[2]).toEqual(allResult[2]); // mixed nulls, no blob
  });

  test('mixed column types in single table', () => {
    const db = makeDb();
    db.exec(
      'CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL, avatar BLOB, deleted INTEGER)',
    );
    db.exec(`INSERT INTO t VALUES (1, 'Alice', 99.5, X'CAFEBABE', 0)`);
    db.exec(`INSERT INTO t VALUES (2, 'Bob', NULL, NULL, 1)`);
    db.exec(`INSERT INTO t VALUES (3, '日本', 0.001, X'', 0)`);

    const stmt = db.prepare('SELECT * FROM t');
    const allResult = stmt.all<Record<string, unknown>>();
    const buf = stmt.allBuf();
    const bufResult = decodeBuf<Record<string, unknown>>(buf, [
      'id',
      'name',
      'score',
      'avatar',
      'deleted',
    ]);

    expect(bufResult.length).toBe(allResult.length);
    for (let i = 0; i < allResult.length; i++) {
      for (const key of ['id', 'name', 'score', 'deleted']) {
        expect(bufResult[i][key]).toEqual(allResult[i][key]);
      }
      // Blob comparison
      const bufBlob = bufResult[i].avatar;
      const allBlob = allResult[i].avatar;
      if (bufBlob === null) {
        expect(allBlob).toBeNull();
      } else {
        expect(
          Buffer.from(bufBlob as Buffer).equals(Buffer.from(allBlob as Buffer)),
        ).toBe(true);
      }
    }
  });

  test('empty result set', () => {
    const db = makeDb();
    db.exec('CREATE TABLE t (id INTEGER)');

    const stmt = db.prepare('SELECT * FROM t');
    const allResult = stmt.all();
    const buf = stmt.allBuf();
    const bufResult = decodeBuf(buf, ['id']);

    expect(bufResult).toEqual(allResult);
    expect(bufResult).toEqual([]);
  });

  test('parameterized queries', () => {
    const db = makeDb();
    db.exec('CREATE TABLE t (id INTEGER, val TEXT)');
    db.exec(`INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')`);

    const stmt = db.prepare('SELECT * FROM t WHERE id > ?');
    const allResult = stmt.all(1);
    const buf = stmt.allBuf(1);
    const bufResult = decodeBuf(buf, ['id', 'val']);

    expect(bufResult).toEqual(allResult);
    expect(bufResult.length).toBe(2);
  });
});
