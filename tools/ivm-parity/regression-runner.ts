import {execSync} from 'node:child_process';
// Re-runs every divergent AST in all-divergences.json against TS + RS, reports
// per-bucket pass rate. Used in the fix-in-loop workflow: after a fix lands,
// re-invoke this to see which shapes now match TS.
//
// Default mode: hydrate-only (uses harness-fuzz.runOneAst). Cheap, parallel,
// catches anything the hydrate path emits. Most "advance-only" divergences
// have a hydrate counterpart at scale.
//
// Mode=advance: writes the catalog's ASTs as a corpus and shells out to
// harness-advance-coverage.ts. Slow but complete.
//
// Usage:
//   npx tsx regression-runner.ts                       # hydrate-only, all shapes
//   BUCKET=H-related-no-EXISTS npx tsx regression-runner.ts
//   MODE=advance npx tsx regression-runner.ts
import {readFileSync, writeFileSync, existsSync} from 'node:fs';
import {runOneAst} from './harness-fuzz.ts';

const TS_URL =
  process.env.PARITY_TS_URL ?? 'ws://localhost:4858/sync/v49/connect';
const RS_URL =
  process.env.PARITY_RS_URL ?? 'ws://localhost:4868/sync/v50/connect';
const MODE = (process.env.MODE ?? 'hydrate').toLowerCase();
const BUCKET_FILTER = process.env.BUCKET ?? null;
const LIMIT = Number(process.env.LIMIT ?? 0);

const cat = JSON.parse(readFileSync('all-divergences.json', 'utf8'));
let shapes = cat.shapes as Array<{
  canonicalAstHash: string;
  bucket: string;
  features: string[];
  ast: any;
  ids: string[];
}>;

if (BUCKET_FILTER) shapes = shapes.filter(s => s.bucket === BUCKET_FILTER);
if (LIMIT > 0) shapes = shapes.slice(0, LIMIT);

console.error(
  `[regression] mode=${MODE} bucket=${BUCKET_FILTER ?? 'ALL'} shapes=${shapes.length}`,
);

if (MODE === 'advance') {
  // Write a corpus file and shell out to harness-advance-coverage.ts
  const corpus = shapes.map((s, i) => ({
    id: `regress_${String(i).padStart(4, '0')}_${s.canonicalAstHash.slice(0, 8)}`,
    ast: s.ast,
    zql: `// regression catalog idx=${i} bucket=${s.bucket}`,
  }));
  writeFileSync('regression_corpus.json', JSON.stringify(corpus, null, 2));
  console.error(
    `[regression] wrote regression_corpus.json with ${corpus.length} ASTs`,
  );
  console.error(
    `[regression] now run: CORPUS_FILE=regression_corpus.json BATCH_SIZE=10 npm run sweep:advance`,
  );
  process.exit(0);
}

// MODE === 'hydrate' — call runOneAst per shape, aggregate
type ShapeResult = {
  bucket: string;
  status: 'ok' | 'diverge' | 'error';
  reason?: string;
};
const results: ShapeResult[] = [];

for (let i = 0; i < shapes.length; i++) {
  const s = shapes[i];
  if (i % 20 === 0 && i > 0) {
    const sofar = results.reduce(
      (acc, r) => ((acc[r.status] = (acc[r.status] ?? 0) + 1), acc),
      {} as Record<string, number>,
    );
    console.error(
      `[regression] ${i}/${shapes.length} ok=${sofar.ok ?? 0} diverge=${sofar.diverge ?? 0} error=${sofar.error ?? 0}`,
    );
  }
  try {
    const r = await runOneAst({ast: s.ast, tsUrl: TS_URL, rsUrl: RS_URL});
    if (r.status === 'ok') results.push({bucket: s.bucket, status: 'ok'});
    else if (r.status === 'diverge')
      results.push({
        bucket: s.bucket,
        status: 'diverge',
        reason: r.divergence.canonicalKey,
      });
    else results.push({bucket: s.bucket, status: 'error', reason: r.error});
  } catch (e) {
    results.push({
      bucket: s.bucket,
      status: 'error',
      reason: (e as Error).message,
    });
  }
}

// Aggregate per bucket
const perBucket = new Map<
  string,
  {ok: number; diverge: number; error: number}
>();
for (const r of results) {
  if (!perBucket.has(r.bucket))
    perBucket.set(r.bucket, {ok: 0, diverge: 0, error: 0});
  perBucket.get(r.bucket)![r.status]++;
}

console.log('\n=== REGRESSION RUN SUMMARY ===');
console.log(`mode=${MODE} total_shapes=${shapes.length}`);
const total = {ok: 0, diverge: 0, error: 0};
for (const r of results) total[r.status]++;
console.log(
  `overall: ok=${total.ok} diverge=${total.diverge} error=${total.error}`,
);
console.log('\nper bucket:');
const sorted = [...perBucket.entries()].sort();
for (const [b, c] of sorted) {
  const pct =
    c.ok + c.diverge > 0
      ? ((c.ok / (c.ok + c.diverge)) * 100).toFixed(0)
      : 'n/a';
  console.log(
    `  ${b.padEnd(40)} ok=${String(c.ok).padStart(3)} diverge=${String(c.diverge).padStart(3)} error=${String(c.error).padStart(3)} pass=${pct}%`,
  );
}

// Persist for next-iteration comparison
writeFileSync(
  'regression-runner-last.json',
  JSON.stringify(
    {
      mode: MODE,
      total,
      perBucket: Object.fromEntries(perBucket),
      at: new Date().toISOString(),
    },
    null,
    2,
  ),
);
console.log(
  '\nWrote regression-runner-last.json (use to diff against next run)',
);
process.exit(0);
