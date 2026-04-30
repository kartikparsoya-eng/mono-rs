//! TS spec: `packages/zql/src/ivm/union-fan-out.ts` (57 LOC) — protocol-handoff
//! fork paired with UnionFanIn.
//!
//! # TS↔Rust port mapping
//!
//! | TS line                  | TS construct                                                          | Rust intent |
//! | ------------------------ | --------------------------------------------------------------------- | ----------- |
//! | `union-fan-out.ts:11-26` | constructor — store input, register self as input.output             | `UnionFanOutOperator::new(input)` — store input, leave fan_in unset. |
//! | `union-fan-out.ts:22-25` | `setFanIn(fanIn)` — assert not already set                            | `set_fan_in(...)` — `assert!(self.fan_in.is_none())`. |
//! | `union-fan-out.ts:27-33` | `*push(change)` — fanOutStartedPushing → push to all → fanOutDonePushing | `fn push(&mut self, change: Change) -> Vec<Change>` — call started_pushing, push to outputs, call done_pushing, append result. |
//! | `union-fan-out.ts:35-37` | `setOutput(out)` — outputs.push(out)                                  | `add_output(out)` — push into Vec<Arc<Mutex<dyn Operator>>>. |
//! | `union-fan-out.ts:43-45` | `fetch(req)` — delegates to input                                     | `fn fetch(&mut self, req: &FetchRequest) -> Vec<Node>` — `self.input.fetch(req)`. |
//! | `union-fan-out.ts:47-56` | `destroy()` — refcount to outputs.len()                               | Mirror with `destroy_count: AtomicUsize`. |
//!
//! # Implementation status
//!
//! Wave 0 stub — struct skeleton + new() only.
//! Wave 2 implements `Operator` + protocol handoff with `Arc<Mutex<UnionFanInOperator>>`
//! reference + ≥10 unit tests.

use std::sync::{
    atomic::AtomicUsize,
    Arc, Mutex,
};

use crate::operator::Operator;
use crate::union_fan_in_op::UnionFanInOperator;

/// UnionFanOut operator — push fork paired with UnionFanIn.
///
/// TS spec: `packages/zql/src/ivm/union-fan-out.ts` lines 11-57.
///
/// Wave 0 stub. Wave 2 implements `Operator` trait + ≥10 unit tests.
#[allow(dead_code)]
pub struct UnionFanOutOperator {
    /// TS line 14: input source (e.g. parent operator chain).
    pub(crate) input: Box<dyn Operator>,
    /// TS line 15: registered outputs. Each output is one branch.
    /// Wave 2: switch to `Vec<Arc<Mutex<dyn Operator>>>`.
    pub(crate) outputs: Vec<Arc<Mutex<dyn Operator>>>,
    /// TS line 13: paired UnionFanIn for protocol handoff. Set via `set_fan_in`.
    pub(crate) fan_in: Option<Arc<Mutex<UnionFanInOperator>>>,
    /// TS line 12: destroy refcount.
    pub(crate) destroy_count: AtomicUsize,
}

impl UnionFanOutOperator {
    #[allow(dead_code)]
    pub fn new(input: Box<dyn Operator>) -> Self {
        Self {
            input,
            outputs: vec![],
            fan_in: None,
            destroy_count: AtomicUsize::new(0),
        }
    }
}

#[cfg(test)]
mod tests {
    // Wave 2 fills in tests. Wave 0 leaves this empty.
}
