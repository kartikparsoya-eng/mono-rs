#!/usr/bin/env node
/**
 * Timed snapshotter test comparison (D-23)
 * Runs the snapshotter test suite and validates wall-clock time.
 */
const { execSync } = require('node:child_process');
const path = require('node:path');

const MONO_ROOT = path.resolve(__dirname, '../../..');
const TEST_FILE = 'packages/zero-cache/src/services/view-syncer/snapshotter.test.ts';
const MAX_DURATION_MS = 30000; // 30s ceiling for regression detection

console.log('Running snapshotter test suite (timed)...');
console.log(`  File: ${TEST_FILE}\n`);

const start = Date.now();
try {
  execSync(`npx vitest run ${TEST_FILE} --reporter=dot`, {
    cwd: MONO_ROOT,
    stdio: 'pipe',
    timeout: 60000,
  });
} catch (e) {
  console.error('Test suite FAILED:');
  console.error(e.stdout?.toString().slice(-2000) || e.message);
  process.exit(1);
}
const elapsed = Date.now() - start;

console.log(`  Duration: ${elapsed}ms`);
console.log(`  Threshold: ${MAX_DURATION_MS}ms`);

if (process.argv.includes('--assert')) {
  if (elapsed > MAX_DURATION_MS) {
    console.error(`\n  FAIL: snapshotter tests took ${elapsed}ms (> ${MAX_DURATION_MS}ms threshold)`);
    process.exit(1);
  }
  console.log(`\n  PASS: snapshotter tests completed within threshold`);
}
