// Aggregates all divergences found across this session into a single
// catalog: hydrate-fuzz random ASTs + curated-corpus advance + random-corpus
// advance. Dedupes by canonicalKey/structural shape, classifies by features,
// emits a master list useful for triage and future regression tests.
import {readFileSync, writeFileSync, existsSync} from 'node:fs';

type Entry = {
  source: string;
  id: string;
  ast: any;
  zql?: string;
  divergence?: any;
  delta?: {ts: any; rs: any};
  bucket?: string;
  features?: string[];
};

const all: Entry[] = [];

// 1. Hydrate-fuzz random ASTs (from PARITY-DIVERGENCES-CATALOG.md /tmp logs)
if (existsSync('/tmp/multi-seed-fuzz.log')) {
  const txt = readFileSync('/tmp/multi-seed-fuzz.log', 'utf8');
  const lines = txt.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(
      /^\s+- ([0-9a-f]+): unexpected divergence \(canonicalKey=([0-9a-f]+)\)/,
    );
    if (!m) continue;
    const next = lines[i + 1] ?? '';
    const am = next.match(/^\s+AST: (.+)$/);
    if (!am) continue;
    try {
      const ast = JSON.parse(am[1]);
      all.push({
        source: 'hydrate-fuzz-multi-seed',
        id: m[1],
        ast,
        divergence: {canonicalKey: m[2]},
      });
    } catch {}
  }
}
if (existsSync('/tmp/big-sweep.log')) {
  const txt = readFileSync('/tmp/big-sweep.log', 'utf8');
  const lines = txt.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(
      /^\s+- ([0-9a-f]+): unexpected divergence \(canonicalKey=([0-9a-f]+)\)/,
    );
    if (!m) continue;
    const next = lines[i + 1] ?? '';
    const am = next.match(/^\s+AST: (.+)$/);
    if (!am) continue;
    try {
      const ast = JSON.parse(am[1]);
      all.push({
        source: 'hydrate-fuzz-big-sweep',
        id: m[1],
        ast,
        divergence: {canonicalKey: m[2]},
      });
    } catch {}
  }
}

// 2. Advance corpus run (advance_coverage_run.json — most recent overwrites; the
// run before this 200-random one was the 1198-curated run, but advance_coverage
// got overwritten. We can re-derive from advance_parity_gaps.md if needed —
// but the per-AST advance_coverage_run.json contains both.)
if (existsSync('advance_coverage_run.json')) {
  const j = JSON.parse(readFileSync('advance_coverage_run.json', 'utf8'));
  const results = j.results ?? [];
  for (const r of results) {
    if (r?.outcome?.status === 'advance-diverge') {
      all.push({
        source: 'advance-corpus-current',
        id: r.id,
        ast: r.ast,
        zql: r.zql,
        delta: {ts: r.outcome.tsDelta, rs: r.outcome.rsDelta},
      });
    } else if (r?.outcome?.status === 'hydrate-diverge') {
      all.push({
        source: 'advance-corpus-current(hydDiverge)',
        id: r.id,
        ast: r.ast,
        zql: r.zql,
      });
    }
  }
}

function classify(ast: any): {bucket: string; features: string[]} {
  const s = JSON.stringify(ast);
  const features: string[] = [];

  const orCount = (s.match(/"type":"or"/g) ?? []).length;
  const andCount = (s.match(/"type":"and"/g) ?? []).length;
  const existsCount = (s.match(/"correlatedSubquery"/g) ?? []).length;
  const hasNotExists = s.includes('"NOT EXISTS"');
  const hasFlip = s.includes('"flip":true');
  const hasScalar = s.includes('"scalar":true');
  const hasStart = s.includes('"start":');
  const hasLimit = s.includes('"limit":');
  const hasOrderBy = s.includes('"orderBy":');
  const hasRelated = s.includes('"related":[{');
  const hasIsNull = s.includes('"op":"IS"') || s.includes('"op":"IS NOT"');
  const hasNotIn =
    s.includes('"NOT IN"') ||
    s.includes('"NOT LIKE"') ||
    s.includes('"NOT ILIKE"');

  if (orCount > 0) features.push(`or(${orCount})`);
  if (andCount > 0) features.push(`and(${andCount})`);
  if (existsCount > 0) features.push(`exists(${existsCount})`);
  if (hasNotExists) features.push('NOT_EXISTS');
  if (hasFlip) features.push('flip');
  if (hasScalar) features.push('scalar');
  if (hasStart) features.push('start');
  if (hasLimit) features.push('limit');
  if (hasOrderBy) features.push('orderBy');
  if (hasRelated) features.push('related[]');
  if (hasIsNull) features.push('IS_NULL');
  if (hasNotIn) features.push('NOT_IN/LIKE');

  // Bucket: AST shape category
  let bucket: string;
  if (hasFlip) bucket = 'B7-flipped-EXISTS';
  else if (hasScalar) bucket = 'scalar-EXISTS';
  else if (hasNotExists) bucket = 'NOT-EXISTS';
  else if (orCount >= 2 && existsCount > 0) bucket = 'A-nested-OR-with-EXISTS';
  else if (orCount > 0 && andCount > 0 && existsCount > 0)
    bucket = 'B-OR-of-AND-of-EXISTS';
  else if (orCount > 0 && existsCount >= 2) bucket = 'C-OR-with-multi-EXISTS';
  else if (orCount > 0 && existsCount > 0) bucket = 'D-simple-OR-with-EXISTS';
  else if (existsCount > 0 && hasRelated)
    bucket = 'E-EXISTS-with-relatedDecoration';
  else if (existsCount > 0) bucket = 'F-plain-EXISTS';
  else if (hasNotIn) bucket = 'G-NOT-IN/LIKE-no-EXISTS';
  else if (hasRelated) bucket = 'H-related-no-EXISTS';
  else bucket = 'Z-other';
  return {bucket, features};
}

for (const e of all) {
  const c = classify(e.ast);
  e.bucket = c.bucket;
  e.features = c.features;
}

// Dedupe by AST canonical hash (sorted-keys serialization)
function canon(o: any): string {
  if (o === null || typeof o !== 'object') return JSON.stringify(o);
  if (Array.isArray(o)) return '[' + o.map(canon).join(',') + ']';
  return (
    '{' +
    Object.keys(o)
      .sort()
      .map(k => JSON.stringify(k) + ':' + canon(o[k]))
      .join(',') +
    '}'
  );
}

const byHash = new Map<string, Entry[]>();
for (const e of all) {
  const h = canon(e.ast);
  if (!byHash.has(h)) byHash.set(h, []);
  byHash.get(h)!.push(e);
}

const buckets = new Map<string, Entry[]>();
for (const [, es] of byHash) {
  const e = es[0];
  const b = e.bucket!;
  if (!buckets.has(b)) buckets.set(b, []);
  buckets.get(b)!.push(e);
}

const lines: string[] = [];
lines.push('# Rust IVM — All Known Divergences (Master Catalog)');
lines.push('');
lines.push(`Total divergent observations across all sweeps: **${all.length}**`);
lines.push(`Unique AST shapes (after canonical-key dedup): **${byHash.size}**`);
lines.push('');
lines.push('Sources:');
const bySource: Record<string, number> = {};
for (const e of all) bySource[e.source] = (bySource[e.source] ?? 0) + 1;
for (const [s, n] of Object.entries(bySource)) lines.push(`- ${s}: ${n}`);
lines.push('');
lines.push('## Bucket distribution (deduped)');
lines.push('');
lines.push('| Bucket | Count | % of unique |');
lines.push('|---|---|---|');
const sorted = [...buckets.entries()].sort((a, b) => b[1].length - a[1].length);
for (const [b, es] of sorted) {
  lines.push(
    `| ${b} | ${es.length} | ${((es.length / byHash.size) * 100).toFixed(0)}% |`,
  );
}
lines.push('');
lines.push('## Per-bucket samples (first 5 unique shapes)');
lines.push('');
for (const [b, es] of sorted) {
  lines.push(`### ${b} (${es.length} unique shapes)`);
  lines.push('');
  for (const e of es.slice(0, 5)) {
    lines.push(`#### ${e.id} _(source: ${e.source})_`);
    if (e.features?.length) lines.push(`- features: ${e.features.join(', ')}`);
    if (e.divergence?.canonicalKey)
      lines.push(`- canonicalKey: \`${e.divergence.canonicalKey}\``);
    if (e.delta) {
      const tsKeys = Object.keys(e.delta.ts ?? {});
      const rsKeys = Object.keys(e.delta.rs ?? {});
      lines.push(`- TS delta tables: \`${tsKeys.join(', ')}\``);
      lines.push(`- RS delta tables: \`${rsKeys.join(', ')}\``);
      const onlyTs = tsKeys.filter(k => !rsKeys.includes(k));
      const onlyRs = rsKeys.filter(k => !tsKeys.includes(k));
      if (onlyTs.length) lines.push(`- only-TS: \`${onlyTs.join(', ')}\``);
      if (onlyRs.length)
        lines.push(
          `- only-RS: \`${onlyRs.join(', ')}\` ← may be alias-leak bug`,
        );
    }
    lines.push('```json');
    lines.push(JSON.stringify(e.ast, null, 2));
    lines.push('```');
    lines.push('');
  }
}

// Also save a flat .json for programmatic use later
writeFileSync(
  'all-divergences.json',
  JSON.stringify(
    {
      total: all.length,
      uniqueShapes: byHash.size,
      sources: bySource,
      buckets: Object.fromEntries(sorted.map(([b, es]) => [b, es.length])),
      shapes: [...byHash.entries()].map(([h, es]) => ({
        canonicalAstHash: h.slice(0, 32),
        bucket: es[0].bucket,
        features: es[0].features,
        sources: [...new Set(es.map(e => e.source))],
        ids: es.map(e => e.id),
        ast: es[0].ast,
        zql: es[0].zql ?? null,
        firstDelta: es[0].delta ?? null,
      })),
    },
    null,
    2,
  ),
);

writeFileSync('ALL-DIVERGENCES-CATALOG.md', lines.join('\n'));
console.log(`Wrote ALL-DIVERGENCES-CATALOG.md and all-divergences.json`);
console.log(`Unique shapes: ${byHash.size}, Buckets: ${buckets.size}`);
for (const [b, es] of sorted) console.log(`  ${b}: ${es.length}`);
