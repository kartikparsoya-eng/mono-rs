/**
 * Bootstrap for worker threads — registers tsx loader before importing worker code.
 * This is needed because tsx loader hooks don't propagate to worker_threads automatically.
 */
import {register} from 'module';
import {pathToFileURL} from 'url';

// Register tsx's ESM loader
register('tsx/esm', pathToFileURL('./'));

// Now import the actual worker
await import('./parallel-fanout-worker.ts');
