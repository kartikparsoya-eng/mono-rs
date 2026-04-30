// Parses advance_parity_gaps.md and categorizes the 259 advance-path
// divergences by AST shape and divergence direction.
import {readFileSync, writeFileSync} from 'node:fs';

const txt = readFileSync('advance_parity_gaps.md', 'utf8');

type Entry = {
  id: string;
  zql: string;
  tables: Array<{name: string; ts: number; rs: number}>;
};

const entries: Entry[] = [];
const sections = txt.split(/^### /gm).slice(1);

for (const sec of sections) {
  const idMatch = sec.match(/^([A-Za-z_0-9]+)/);
  if (!idMatch) continue;
  const id = idMatch[1];
  const zqlMatch = sec.match(/```ts\n([\s\S]+?)\n```/);
  const zql = zqlMatch?.[1].trim() ?? '';
  const tables: Entry['tables'] = [];
  const tblRegex =
    /^  ([a-z_0-9]+): ts=(\d+) change\(s\)  rs=(\d+) change\(s\)/gm;
  let m;
  while ((m = tblRegex.exec(sec)) !== null) {
    tables.push({name: m[1], ts: Number(m[2]), rs: Number(m[3])});
  }
  if (tables.length === 0) continue;
  entries.push({id, zql, tables});
}

// Bucket by AST shape (extracted from zql snippet)
function bucket(zql: string): string {
  if (zql.includes('not(exists(') || zql.includes('not exists'))
    return 'NOT-exists';
  if (zql.includes("exists('") && zql.includes(', {flip: true'))
    return 'flipped EXISTS';
  if (zql.includes("exists('") && zql.includes(', {scalar: true'))
    return 'scalar EXISTS';
  if (zql.includes('whereExists(')) return 'whereExists';
  if (zql.includes("exists('")) return 'where(exists(...))';
  if (zql.includes('related(')) return 'related (no exists)';
  if (zql.includes('.where(')) return 'plain filter';
  return 'other';
}

const buckets = new Map<string, Entry[]>();
for (const e of entries) {
  const b = bucket(e.zql);
  if (!buckets.has(b)) buckets.set(b, []);
  buckets.get(b)!.push(e);
}

// Direction stats
let tsExtra = 0,
  rsExtra = 0,
  both = 0,
  aliasOnly = 0;
for (const e of entries) {
  for (const t of e.tables) {
    if (t.name.startsWith('zsubq_')) aliasOnly++;
    else if (t.ts > t.rs) tsExtra++;
    else if (t.rs > t.ts) rsExtra++;
    else both++;
  }
}

const lines: string[] = [];
lines.push('# Advance-Path Divergences — Full Corpus (1198 ASTs)');
lines.push('');
lines.push(`- Total divergent ASTs: **${entries.length}**`);
lines.push(
  `- Per-table change-count rows: ${tsExtra + rsExtra + both + aliasOnly}`,
);
lines.push(`  - TS emits extra rows: ${tsExtra}`);
lines.push(`  - RS emits extra rows: ${rsExtra}`);
lines.push(
  `  - "zsubq_*" aliased rows (likely harness alias artifact): ${aliasOnly}`,
);
lines.push(`  - Equal counts (other diff): ${both}`);
lines.push('');
lines.push('## Bucket distribution');
lines.push('');
lines.push('| Bucket | Count | % |');
lines.push('|---|---|---|');
for (const [b, es] of [...buckets.entries()].sort(
  (a, b) => b[1].length - a[1].length,
)) {
  lines.push(
    `| ${b} | ${es.length} | ${((es.length / entries.length) * 100).toFixed(0)}% |`,
  );
}
lines.push('');
lines.push('## Sample per bucket (first 3)');
lines.push('');
for (const [b, es] of [...buckets.entries()].sort(
  (a, b) => b[1].length - a[1].length,
)) {
  lines.push(`### ${b} (${es.length})`);
  lines.push('');
  for (const e of es.slice(0, 3)) {
    lines.push(`- **${e.id}**: ${e.zql.replace(/\s+/g, ' ').slice(0, 120)}`);
    for (const t of e.tables) {
      const aliasMark = t.name.startsWith('zsubq_') ? ' (alias artifact?)' : '';
      lines.push(`    - \`${t.name}\`: ts=${t.ts} rs=${t.rs}${aliasMark}`);
    }
  }
  lines.push('');
}

writeFileSync('ADVANCE-DIVERGENCES-CATALOG.md', lines.join('\n'));
console.log(`Wrote ADVANCE-DIVERGENCES-CATALOG.md`);
console.log(
  `Buckets: ${[...buckets.entries()].map(([b, es]) => `${b}=${es.length}`).join(', ')}`,
);
console.log(
  `Direction: ts-extra=${tsExtra}  rs-extra=${rsExtra}  alias=${aliasOnly}  both=${both}`,
);
