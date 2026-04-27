import {mergeConfig} from 'vitest/config';
import {benchConfig} from '../shared/src/tool/vitest-config.ts';

export default mergeConfig(benchConfig, {
  test: {
    name: 'zero-cache/bench',
    browser: {
      enabled: false,
    },
    // Disable Rust IVM in PipelineDriver so TS benchmarks use pure TypeScript
    // operators. Rust benchmarks call RustPipeline NAPI directly.
    env: {
      ZERO_DISABLE_RUST_IVM: '1',
    },
  },
});
