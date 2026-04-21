---
phase: 20
plan: '04'
title: 'Pipeline Builder and NAPI Bridge'
wave: 3
depends_on: ['01', '02', '03']
files_modified:
  - packages/zero-ivm-rs/src/pipeline.rs
  - packages/zero-ivm-rs/src/lib.rs
  - packages/zero-ivm-rs/src/Cargo.toml
requirements_addressed: [OPR-03]
autonomous: true
---

# Plan 04: Pipeline Builder and NAPI Bridge

<objective>
Build a pipeline builder that constructs Rust operator trees from JSON config descriptors sent by TS, and expose it via napi as an opaque Pipeline handle with fetch() and push() methods.
</objective>

<threat_model>
| Threat | Severity | Mitigation |
|--------|----------|------------|
| Malformed JSON config causes panic | medium | All JSON parsing wrapped in Result; napi returns Error not panic |
| Config schema drift between TS and Rust | medium | Config types use serde with deny_unknown_fields; version field for forward compat |
| Pipeline handle use-after-free via napi | low | napi prevents this — Pipeline is a normal reference-counted JS object |
</threat_model>

<tasks>

<task id="20-04-01">
<title>Define pipeline config JSON schema and deserializer</title>
<read_first>
- packages/zero-ivm-rs/src/types.rs
- packages/zero-ivm-rs/src/filter_op.rs
- packages/zero-ivm-rs/src/join_op.rs
- packages/zero-ivm-rs/src/take_op.rs
- packages/zero-ivm-rs/src/exists_op.rs
- packages/zero-ivm-rs/src/skip_op.rs
- packages/zero-ivm-rs/src/cap_op.rs
- packages/zql/src/builder/builder.ts
</read_first>
<action>
Create `packages/zero-ivm-rs/src/pipeline.rs` with config types:

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum OperatorConfig {
    #[serde(rename = "source")]
    Source {
        table_name: String,
        columns: Vec<String>,
        primary_key: Vec<String>,
        sort: Vec<(String, String)>, // (field, "asc"|"desc")
    },
    #[serde(rename = "filter")]
    Filter {
        predicate: serde_json::Value,
    },
    #[serde(rename = "join")]
    Join {
        parent_key: Vec<String>,
        child_key: Vec<String>,
        relationship_name: String,
        child: Vec<OperatorConfig>, // child pipeline config
    },
    #[serde(rename = "take")]
    Take {
        limit: usize,
        sort: Vec<(String, String)>,
        partition_key: Option<Vec<String>>,
    },
    #[serde(rename = "exists")]
    Exists {
        relationship_name: String,
        not_exists: bool,
        parent_key: Vec<String>,
        child_key: Vec<String>,
        child: Vec<OperatorConfig>,
    },
    #[serde(rename = "skip")]
    Skip {
        bound_row: serde_json::Value,
        exclusive: bool,
        sort: Vec<(String, String)>,
    },
    #[serde(rename = "cap")]
    Cap {
        limit: usize,
        primary_key: Vec<String>,
        partition_key: Option<Vec<String>>,
    },
}
```

Implement `build_operator(configs: &[OperatorConfig]) -> Result<Box<dyn Operator>>` that walks the config array and constructs the operator tree bottom-up. The Source config creates a placeholder `SourceOperator` that will be replaced with RustTableSource in Phase 21 — for now it holds pre-loaded data for testing.

```rust
pub struct SourceOperator {
    rows: Vec<Node>,
    sort: Vec<SortSpec>,
}

impl Operator for SourceOperator {
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node> {
        // Apply constraint filter, start bound, reverse, return matching rows
    }
    fn push(&mut self, _change: Change) -> Vec<Change> {
        vec![] // Source doesn't transform pushes — they originate here
    }
}
```

Add unit tests: deserialize each operator config variant from JSON string.
</action>
<acceptance_criteria>

- `packages/zero-ivm-rs/src/pipeline.rs` contains `pub enum OperatorConfig`
- `pipeline.rs` contains variants `Source`, `Filter`, `Join`, `Take`, `Exists`, `Skip`, `Cap`
- `pipeline.rs` contains `pub fn build_operator(`
- `pipeline.rs` contains `pub struct SourceOperator`
- `pipeline.rs` contains `#[cfg(test)]` with at least 3 test functions for config deserialization
- `cargo test` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-04-02">
<title>Expose Pipeline as napi class</title>
<read_first>
- packages/zero-ivm-rs/src/pipeline.rs
- packages/zero-ivm-rs/src/filter.rs (for napi pattern reference)
- packages/zero-ivm-rs/src/take_state.rs (for napi class pattern reference)
</read_first>
<action>
Add napi bridge to `packages/zero-ivm-rs/src/pipeline.rs`:

```rust
use napi::bindgen_prelude::*;
use napi_derive::napi;

#[napi]
pub struct Pipeline {
    operator: Box<dyn Operator>,
}

#[napi]
impl Pipeline {
    /// Build a pipeline from JSON config.
    /// config_json: JSON string of Vec<OperatorConfig>
    #[napi(factory)]
    pub fn build(config_json: String) -> Result<Self> {
        let configs: Vec<OperatorConfig> = serde_json::from_str(&config_json)
            .map_err(|e| Error::from_reason(format!("Invalid pipeline config: {}", e)))?;
        let operator = build_operator(&configs)
            .map_err(|e| Error::from_reason(format!("Failed to build pipeline: {}", e)))?;
        Ok(Self { operator })
    }

    /// Fetch data. request_json: JSON string of FetchRequest.
    /// Returns JSON string of Vec<Node>.
    #[napi]
    pub fn fetch(&mut self, request_json: String) -> Result<String> {
        let req: FetchRequest = serde_json::from_str(&request_json)
            .map_err(|e| Error::from_reason(format!("Invalid fetch request: {}", e)))?;
        let nodes = self.operator.fetch(&req);
        serde_json::to_string(&nodes)
            .map_err(|e| Error::from_reason(format!("Serialization error: {}", e)))
    }

    /// Push a change. change_json: JSON string of Change.
    /// Returns JSON string of Vec<Change>.
    #[napi]
    pub fn push(&mut self, change_json: String) -> Result<String> {
        let change: Change = serde_json::from_str(&change_json)
            .map_err(|e| Error::from_reason(format!("Invalid change: {}", e)))?;
        let changes = self.operator.push(change);
        serde_json::to_string(&changes)
            .map_err(|e| Error::from_reason(format!("Serialization error: {}", e)))
    }
}
```

Note: Pipeline uses JSON string input/output for now. Phase 25 will optimize to binary buffers. The `Operator` trait object is not `Send` across napi boundary (napi handles are main-thread), but the trait itself is `Send` to allow Phase 22/24 to use pipelines on Rayon threads without napi.
</action>
<acceptance_criteria>

- `pipeline.rs` contains `#[napi] pub struct Pipeline`
- `pipeline.rs` contains `pub fn build(config_json: String) -> Result<Self>`
- `pipeline.rs` contains `pub fn fetch(&mut self, request_json: String) -> Result<String>`
- `pipeline.rs` contains `pub fn push(&mut self, change_json: String) -> Result<String>`
- `cargo check` in `packages/zero-ivm-rs/` passes
  </acceptance_criteria>
  </task>

<task id="20-04-03">
<title>Integration tests: build and run pipelines end-to-end</title>
<read_first>
- packages/zero-ivm-rs/src/pipeline.rs
- packages/zero-ivm-rs/src/types.rs
</read_first>
<action>
Add integration tests to `packages/zero-ivm-rs/src/pipeline.rs` (in `#[cfg(test)]` block):

1. **Filter pipeline test**: Build Source + Filter pipeline from JSON config. Pre-load source with 5 rows. Fetch with no constraint → returns only rows matching filter predicate.

2. **Join pipeline test**: Build Source(parent) + Join(Source(child)) pipeline. Pre-load parent with 3 rows, child with 6 rows (2 per parent). Fetch → returns 3 parent nodes each with 2 child relationships.

3. **Take pipeline test**: Build Source + Take(limit=2) pipeline. Pre-load with 5 sorted rows. Fetch → returns first 2 rows. Push Add of a row before the bound → returns Add + Remove (displacement).

4. **Exists pipeline test**: Build Source(parent) + Exists(child) pipeline. Pre-load parent with 3 rows, child with rows for 2 parents. Fetch → returns only 2 parents that have children.

5. **Push propagation test**: Build Source + Filter + Take pipeline. Push an Add change through → verify filter evaluates, then take bounds are updated.

Each test constructs the operator tree directly (not via JSON) to test the Operator trait composition independent of the config deserializer.
</action>
<acceptance_criteria>

- `pipeline.rs` contains at least 5 integration test functions in `#[cfg(test)]`
- Tests cover Filter, Join, Take, Exists, and multi-operator pipeline
- `cargo test` in `packages/zero-ivm-rs/` passes with all new tests
  </acceptance_criteria>
  </task>

<task id="20-04-04">
<title>Register pipeline module in lib.rs</title>
<read_first>
- packages/zero-ivm-rs/src/lib.rs
</read_first>
<action>
Add `pub mod pipeline;` to `packages/zero-ivm-rs/src/lib.rs`.
</action>
<acceptance_criteria>
- `packages/zero-ivm-rs/src/lib.rs` contains `pub mod pipeline;`
- `cargo test` in `packages/zero-ivm-rs/` passes
</acceptance_criteria>
</task>

</tasks>

<verification>
```bash
cd packages/zero-ivm-rs && cargo test
cd packages/zero-ivm-rs && cargo clippy -- -D warnings
cd packages/zero-ivm-rs && cargo test pipeline
```
</verification>

<must_haves>

- Pipeline builder constructs operator tree from JSON config descriptors
- All 6 operator types (Filter, Join, Take, Exists, Skip, Cap) constructible from config
- SourceOperator placeholder supports pre-loaded data for testing
- napi Pipeline class exposes build(), fetch(), push() methods
- Integration tests verify multi-operator pipeline composition
- JSON serialization round-trips correctly for Node and Change types
  </must_haves>
