import {trace, type Attributes} from '@opentelemetry/api';
import type {LogContext} from '@rocicorp/logger';
import {
  Database as RustDatabase,
  type Statement as RustStatement,
  type RowIterator as RustRowIterator,
} from 'zqlite-rs';
import {manualSpan} from '../../otel/src/span.ts';
import {version} from '../../otel/src/version.ts';

const tracer = trace.getTracer('view-syncer', version);

// https://www.sqlite.org/pragma.html#pragma_auto_vacuum
const AUTO_VACUUM_INCREMENTAL = 2;

const MB = 1024 * 1024;

function mb(bytes: number): string {
  return (bytes / MB).toFixed(2);
}

export class SqliteError extends Error {
  override name = 'SqliteError';
  code?: string;
}

export type RunResult = {changes: number; lastInsertRowid: number | bigint};

export class Database implements Disposable {
  readonly #db: RustDatabase;
  readonly #threshold: number;
  readonly #lc: LogContext;
  readonly #pageSize: number;

  constructor(
    lc: LogContext,
    path: string,
    options?: {readonly?: boolean; fileMustExist?: boolean},
    slowQueryThreshold = 100,
  ) {
    try {
      this.#lc = lc.withContext('class', 'Database').withContext('path', path);
      this.#db = new RustDatabase(path, options);
      this.#threshold = slowQueryThreshold;

      const [{page_size: pageSize}] = this.pragma<{page_size: number}>(
        'page_size',
      );
      this.#pageSize = pageSize;
    } catch (cause) {
      throw new DatabaseInitError(String(cause), {cause});
    }
  }

  prepare(sql: string): Statement {
    return this.#run(
      'prepare',
      sql,
      () =>
        new Statement(
          this.#lc.withContext('sql', sql),
          {class: 'Statement', sql},
          this.#db.prepare(sql),
          this.#threshold,
        ),
    );
  }

  exec(sql: string): void {
    this.#run('exec', sql, () => this.#db.exec(sql));
  }

  pragma<T = unknown>(sql: string): T[] {
    return this.#run<T[]>('pragma', sql, () => this.#db.pragma(sql) as T[]);
  }

  #bytes(pages: number) {
    return pages * this.#pageSize;
  }

  compact(freeableBytesThreshold: number) {
    const [{freelist_count: freelistCount}] = this.pragma<{
      freelist_count: number;
    }>('freelist_count');

    const freeable = this.#bytes(freelistCount);
    if (freeable < freeableBytesThreshold) {
      this.#lc.debug?.(
        `Not compacting ${this.#db.name}: ${mb(freeable)} freeable MB`,
      );
      return;
    }
    const [{auto_vacuum: autoVacuumMode}] = this.pragma<{auto_vacuum: number}>(
      'auto_vacuum',
    );
    if (autoVacuumMode !== AUTO_VACUUM_INCREMENTAL) {
      this.#lc.warn?.(
        `Cannot compact ${mb(freeable)} MB of ` +
          `${this.#db.name} because AUTO_VACUUM mode is ${autoVacuumMode}.`,
      );
      return;
    }
    const start = Date.now();
    const [{page_count: pageCountBefore}] = this.pragma<{page_count: number}>(
      'page_count',
    );

    this.pragma('incremental_vacuum');

    const [{page_count: pageCountAfter}] = this.pragma<{page_count: number}>(
      'page_count',
    );

    this.#lc.info?.(
      `Compacted ${this.#db.name} from ` +
        `${mb(this.#bytes(pageCountBefore))} MB to ` +
        `${mb(this.#bytes(pageCountAfter))} MB ` +
        `(${Date.now() - start} ms)`,
    );
  }

  unsafeMode(unsafe: boolean) {
    this.#db.unsafeMode(unsafe);
  }

  #run<T>(method: string, sql: string, fn: () => T): T {
    const start = performance.now();
    try {
      return fn();
    } catch (e) {
      if (e instanceof Error) {
        // Strip rusqlite's " in ... at offset N" suffix to match better-sqlite3 format
        const msg = e.message.replace(/ in .+ at offset \d+$/, '');
        const sqliteErr = new SqliteError(`${msg}: ${sql}`);
        sqliteErr.stack = e.stack;
        throw sqliteErr;
      }
      throw e;
    } finally {
      logIfSlow(
        this.#lc.withContext('method', method),
        performance.now() - start,
        {method},
        this.#threshold,
      );
    }
  }

  /**
   * Execute a query and return all rows with typed conversion done in Rust.
   * columnTypes maps column names to their schema types (e.g. 'boolean', 'json').
   */
  queryAll<T>(
    sql: string,
    params: unknown[],
    columnTypes: Record<string, string>,
    tableName: string,
  ): T[] {
    return this.#run('queryAll', sql, () =>
      this.#db.queryAll(sql, params, columnTypes, tableName),
    );
  }

  close(): void {
    const start = Date.now();
    if (!this.#db.readonly) {
      try {
        this.#db.pragma('optimize');
        const elapsed = Date.now() - start;
        if (elapsed > 2) {
          this.#lc.debug?.(`PRAGMA optimized (${elapsed} ms)`);
        }
      } catch (e) {
        this.#lc.warn?.('error running PRAGMA optimize', e);
      }
    }
    this.#db.close();
  }

  transaction<T>(fn: () => T): T {
    return this.#db.transaction(fn) as T;
  }

  get name() {
    return this.#db.name;
  }

  get inTransaction() {
    return this.#db.inTransaction;
  }

  [Symbol.dispose](): void {
    this.close();
  }
}

export class Statement {
  readonly #stmt: RustStatement;
  readonly #lc: LogContext;
  readonly #threshold: number;
  readonly #attrs: Attributes;

  // Stubs for scanStatusV2/scanStatusReset (used by table-source.ts, not needed for Phase 1)
  readonly scanStatus: (...args: unknown[]) => unknown = () => undefined;
  readonly scanStatusReset: () => void = () => {};

  constructor(
    lc: LogContext,
    attrs: Attributes,
    stmt: RustStatement,
    threshold: number,
  ) {
    this.#lc = lc.withContext('class', 'Statement');
    this.#attrs = attrs;
    this.#stmt = stmt;
    this.#threshold = threshold;
  }

  safeIntegers(useBigInt: boolean): this {
    this.#stmt.safeIntegers(useBigInt);
    return this;
  }

  run(...params: unknown[]): RunResult {
    const start = performance.now();
    const ret = this.#stmt.run(params);
    logIfSlow(
      this.#lc.withContext('method', 'run'),
      performance.now() - start,
      {...this.#attrs, method: 'run'},
      this.#threshold,
    );
    return ret as RunResult;
  }

  get<T>(...params: unknown[]): T {
    const start = performance.now();
    const ret = this.#stmt.get(params);
    logIfSlow(
      this.#lc.withContext('method', 'get'),
      performance.now() - start,
      {...this.#attrs, method: 'get'},
      this.#threshold,
    );
    return ret as T;
  }

  all<T>(...params: unknown[]): T[] {
    const start = performance.now();
    const ret = this.#stmt.all(params);
    logIfSlow(
      this.#lc.withContext('method', 'all'),
      performance.now() - start,
      {...this.#attrs, method: 'all'},
      this.#threshold,
    );
    return ret as T[];
  }

  iterate<T>(...params: unknown[]): IterableIterator<T> {
    return new LoggingIterableIterator(
      this.#lc.withContext('method', 'iterate'),
      this.#attrs,
      this.#stmt.iterate(params),
      this.#threshold,
    ) as IterableIterator<T>;
  }
}

class LoggingIterableIterator<T> implements IterableIterator<T> {
  readonly #lc: LogContext;
  readonly #it: RustRowIterator;
  readonly #threshold: number;
  readonly #attrs: Attributes;
  #start: number;
  #sqliteRowTimeSum: number;

  constructor(
    lc: LogContext,
    attrs: Attributes,
    it: RustRowIterator,
    slowQueryThreshold: number,
  ) {
    this.#lc = lc;
    this.#attrs = attrs;
    this.#it = it;
    this.#start = performance.now();
    this.#threshold = slowQueryThreshold;
    this.#sqliteRowTimeSum = 0;
  }

  next(): IteratorResult<T> {
    const start = performance.now();
    const ret = this.#it.next() as IteratorResult<T>;
    const elapsed = performance.now() - start;
    this.#sqliteRowTimeSum += elapsed;
    if (ret.done) {
      this.#log();
    }
    return ret;
  }

  #log() {
    logIfSlow(
      this.#lc.withContext('type', 'total'),
      performance.now() - this.#start,
      {...this.#attrs, type: 'total', method: 'iterate'},
      this.#threshold,
    );
    logIfSlow(
      this.#lc.withContext('type', 'sqlite'),
      this.#sqliteRowTimeSum,
      {...this.#attrs, type: 'sqlite', method: 'iterate'},
      this.#threshold,
    );
  }

  [Symbol.iterator](): IterableIterator<T> {
    return this;
  }

  return(): IteratorResult<T> {
    this.#log();
    // oxlint-disable-next-line @typescript-eslint/no-explicit-any
    return this.#it.return() as any;
  }

  throw(e: unknown): IteratorResult<T> {
    this.#log();
    return {done: true, value: undefined} as IteratorResult<T>;
  }
}

function logIfSlow(
  lc: LogContext,
  elapsed: number,
  attrs: Attributes,
  threshold: number,
): void {
  if (elapsed >= threshold) {
    for (const [key, value] of Object.entries(attrs)) {
      lc = lc.withContext(key, value);
    }
    lc.warn?.('Slow SQLite query', elapsed);
    manualSpan(tracer, 'db.slow-query', elapsed, attrs);
  }
}

/**
 * An error indicating that the Database failed to open. This essentially
 * wraps the TypeError thrown by the better-sqlite3 package with something
 * more specific.
 */
export class DatabaseInitError extends Error {}
