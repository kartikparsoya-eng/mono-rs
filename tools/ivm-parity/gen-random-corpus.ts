// Generate N random ASTs via fast-check sampling and emit them as a
// CorpusEntry-shaped JSON file that harness-advance-coverage.ts can consume.
//
// Usage:
//   N=1000 SEED=42 OUTFILE=random_advance_corpus.json npx tsx gen-random-corpus.ts
//
// Defaults: N=500, SEED=Date.now(), OUTFILE=random_advance_corpus.json
//
// CorpusEntry format mirrors ast_corpus.json: { id, ast, zql }
// where `id` is a synthetic identifier (rand_NNNNN) and `zql` is a stub.

import {createHash} from 'node:crypto';
import {writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import fc from 'fast-check';
import {buildArbitraries} from './arb-ast.ts';
import {schema as paritySchema} from './zero-schema.ts';

const here = dirname(fileURLToPath(import.meta.url));
const N = Number(process.env.N ?? 500);
const SEED = Number(process.env.SEED ?? Date.now());
const OUTFILE = process.env.OUTFILE ?? 'random_advance_corpus.json';
const ARB_MODE = (process.env.ARB ?? 'targeted').toLowerCase();

// buildArbitraries does its own adaptSchema internally. Mirror what
// random-ast-fuzz.ts does at line 335.
const arbs = buildArbitraries(paritySchema as never);
const arb = ARB_MODE === 'base' ? arbs.arbAst : arbs.arbAstWithTargeted;

console.error(
  `[gen-random-corpus] sampling N=${N} seed=${SEED} arb=${ARB_MODE}`,
);
const samples = fc.sample(arb, {numRuns: N, seed: SEED});

const corpus = samples.map((ast, i) => {
  const idHash = createHash('sha1')
    .update(JSON.stringify(ast))
    .digest('hex')
    .slice(0, 8);
  return {
    id: `rand_${String(i).padStart(5, '0')}_${idHash}`,
    ast,
    zql: `// random AST ${i}`, // stub; harness-advance-coverage.ts may read this for display
  };
});

const outPath = OUTFILE.startsWith('/') ? OUTFILE : join(here, OUTFILE);
writeFileSync(outPath, JSON.stringify(corpus, null, 2));
console.error(`[gen-random-corpus] wrote ${corpus.length} ASTs to ${outPath}`);

// Per-table coverage
const tableHits: Record<string, number> = {};
for (const e of corpus) {
  const t = (e.ast as any).table;
  if (t) tableHits[t] = (tableHits[t] ?? 0) + 1;
}
console.error('[gen-random-corpus] per-table:', JSON.stringify(tableHits));
process.exit(0);
