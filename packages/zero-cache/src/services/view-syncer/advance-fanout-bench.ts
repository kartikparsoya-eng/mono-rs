/**
 * Advance Fan-Out Benchmark
 *
 * Measures the Rust Rayon parallelism benefit for advance fan-out by calling
 * the NAPI functions directly — bypassing PipelineDriver/snapshot overhead
 * that dominates the existing rust-ivm-bench.ts.
 *
 * Two axes:
 *   1. Single-VS: rust_fan_out with increasing pipeline count
 *   2. Multi-VS:  rust_dispatch_poke with increasing VS count (the real prod win)
 *
 * Usage:
 *   node --experimental-strip-types --experimental-transform-types \
 *     packages/zero-cache/src/services/view-syncer/advance-fanout-bench.ts
 */

import {createRequire} from 'node:module';

const esmRequire = createRequire(import.meta.url);
const bindings = esmRequire('zqlite-rs');
const rustFanOut: (c: string, p: string) => string = bindings.rustFanOut;
const rustDispatchPoke: (c: string, v: string) => Buffer =
  bindings.rustDispatchPoke;

// ─── Config ──────────────────────────────────────────────────────────────────
const WARMUP = 3;
const ITERATIONS = 20;
const DIFF_ROWS = 200;

// ─── Generate synthetic diff changes (already-parsed JSON shape) ─────────────
function makeChangesJson(n: number): string {
  const changes = [];
  for (let i = 0; i < n; i++) {
    changes.push({
      table: 'issues',
      prevValues: [],
      nextValue: {
        id: `new-${i}`,
        closed: i % 2 === 0 ? false : true,
        title: `Issue number ${i}`,
        priority: i % 5,
      },
      rowKey: {id: `new-${i}`},
    });
  }
  return JSON.stringify(changes);
}

// ─── Generate pipeline configs (filter-only) ────────────────────────────────
function makePipeline(queryId: string, closedValue: boolean) {
  return {
    query_id: queryId,
    source_tables: ['issues'],
    operators: [
      {
        type: 'filter',
        predicate: {
          type: 'simple',
          left: {type: 'column', name: 'closed'},
          op: '=',
          right: {type: 'literal', value: closedValue},
        },
      },
    ],
    primary_key: ['id'],
  };
}

function makePipelinesJson(count: number): string {
  const pipelines = [];
  for (let i = 0; i < count; i++) {
    pipelines.push(makePipeline(`q${i}`, i % 2 === 0));
  }
  return JSON.stringify(pipelines);
}

// ─── TS baseline: sequential filter evaluation ──────────────────────────────
type Change = {
  table: string;
  nextValue: Record<string, unknown> | null;
  rowKey: Record<string, unknown>;
};
type Pipeline = {
  query_id: string;
  source_tables: string[];
  operators: Array<{
    type: string;
    predicate: {
      type: string;
      left: {type: string; name: string};
      op: string;
      right: {type: string; value: unknown};
    };
  }>;
  primary_key: string[];
};

function tsEvaluateFilter(changes: Change[], pipelines: Pipeline[]): number {
  let total = 0;
  for (const pipeline of pipelines) {
    for (const change of changes) {
      if (!pipeline.source_tables.includes(change.table)) continue;
      const row = change.nextValue;
      if (!row) {
        total++;
        continue;
      }
      let pass = true;
      for (const op of pipeline.operators) {
        if (op.type !== 'filter') continue;
        const col = op.predicate.left.name;
        const val = row[col];
        const lit = op.predicate.right.value;
        if (op.predicate.op === '=') {
          if (val !== lit) {
            pass = false;
            break;
          }
        }
      }
      if (pass) total++;
    }
  }
  return total;
}

function median(arr: number[]): number {
  const s = arr.toSorted((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 !== 0 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}

// ─── Benchmark helpers ──────────────────────────────────────────────────────

function benchSingleVS(pipelineCounts: number[]) {
  const changesJson = makeChangesJson(DIFF_ROWS);
  const changesParsed: Change[] = JSON.parse(changesJson);

  const header = `Single-VS fan-out (${DIFF_ROWS} diff rows, ${ITERATIONS} iterations)`;
  const sep = '-'.repeat(header.length);
  // eslint-disable-next-line no-console
  console.log(`\n${header}\n${sep}`);
  // eslint-disable-next-line no-console
  console.log(
    'pipelines'.padStart(10),
    'TS (ms)'.padStart(10),
    'Rust (ms)'.padStart(10),
    'speedup'.padStart(10),
  );

  for (const count of pipelineCounts) {
    const pipelinesJson = makePipelinesJson(count);
    const pipelinesParsed: Pipeline[] = JSON.parse(pipelinesJson);

    // Warmup
    for (let i = 0; i < WARMUP; i++) {
      rustFanOut(changesJson, pipelinesJson);
      tsEvaluateFilter(changesParsed, pipelinesParsed);
    }

    const tsTimes: number[] = [];
    const rustTimes: number[] = [];

    for (let i = 0; i < ITERATIONS; i++) {
      const t0 = performance.now();
      tsEvaluateFilter(changesParsed, pipelinesParsed);
      tsTimes.push(performance.now() - t0);

      const r0 = performance.now();
      rustFanOut(changesJson, pipelinesJson);
      rustTimes.push(performance.now() - r0);
    }

    const tsMs = median(tsTimes);
    const rustMs = median(rustTimes);
    const speedup = tsMs / rustMs;
    // eslint-disable-next-line no-console
    console.log(
      String(count).padStart(10),
      tsMs.toFixed(3).padStart(10),
      rustMs.toFixed(3).padStart(10),
      `${speedup.toFixed(2)}x`.padStart(10),
    );
  }
}

function benchMultiVS(vsCounts: number[], pipelinesPerVS: number) {
  const changesJson = makeChangesJson(DIFF_ROWS);
  const changesParsed: Change[] = JSON.parse(changesJson);

  const header = `Multi-VS dispatch_poke (${DIFF_ROWS} diff rows, ${pipelinesPerVS} pipelines/VS, ${ITERATIONS} iterations)`;
  const sep = '-'.repeat(header.length);
  // eslint-disable-next-line no-console
  console.log(`\n${header}\n${sep}`);
  // eslint-disable-next-line no-console
  console.log(
    'VS count'.padStart(10),
    'total pipes'.padStart(12),
    'TS seq (ms)'.padStart(12),
    'Rust (ms)'.padStart(12),
    'speedup'.padStart(10),
  );

  for (const vsCount of vsCounts) {
    // Build VS pipeline entries for dispatch_poke
    const vsEntries = [];
    const allPipelinesParsed: Pipeline[][] = [];
    for (let v = 0; v < vsCount; v++) {
      const pipelines = [];
      const pipelinesParsed: Pipeline[] = [];
      for (let p = 0; p < pipelinesPerVS; p++) {
        const pipeline = makePipeline(`vs${v}_q${p}`, p % 2 === 0);
        pipelines.push(pipeline);
        pipelinesParsed.push(pipeline);
      }
      vsEntries.push({vs_id: `vs-${v}`, pipelines});
      allPipelinesParsed.push(pipelinesParsed);
    }
    const vsPipelinesJson = JSON.stringify(vsEntries);

    // Warmup
    for (let i = 0; i < WARMUP; i++) {
      rustDispatchPoke(changesJson, vsPipelinesJson);
      for (const pp of allPipelinesParsed) {
        tsEvaluateFilter(changesParsed, pp);
      }
    }

    const tsTimes: number[] = [];
    const rustTimes: number[] = [];

    for (let i = 0; i < ITERATIONS; i++) {
      // TS: sequential fan-out across all VS (simulates Node.js event loop)
      const t0 = performance.now();
      for (const pp of allPipelinesParsed) {
        tsEvaluateFilter(changesParsed, pp);
      }
      tsTimes.push(performance.now() - t0);

      // Rust: single dispatch_poke call (Rayon parallelizes across VS + pipelines)
      const r0 = performance.now();
      rustDispatchPoke(changesJson, vsPipelinesJson);
      rustTimes.push(performance.now() - r0);
    }

    const tsMs = median(tsTimes);
    const rustMs = median(rustTimes);
    const speedup = tsMs / rustMs;
    const totalPipes = vsCount * pipelinesPerVS;
    // eslint-disable-next-line no-console
    console.log(
      String(vsCount).padStart(10),
      String(totalPipes).padStart(12),
      tsMs.toFixed(3).padStart(12),
      rustMs.toFixed(3).padStart(12),
      `${speedup.toFixed(2)}x`.padStart(10),
    );
  }
}

// ─── Main ────────────────────────────────────────────────────────────────────
// eslint-disable-next-line no-console
console.log('Advance Fan-Out Benchmark');
// eslint-disable-next-line no-console
console.log(`Rayon threads: ${bindings.rayonThreadCount?.() ?? 'unknown'}`);

benchSingleVS([1, 4, 16, 64, 256, 1024]);
benchMultiVS([1, 10, 50, 100, 500, 1000], 10);
benchMultiVS([1, 10, 50, 100, 500], 50);
