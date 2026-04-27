use crate::types::{Change, FetchRequest, Node};

pub trait Operator: Send {
    /// Fetch data matching the request. Returns nodes in sort order.
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node>;

    /// Push an incremental change through this operator.
    /// Returns output changes to propagate downstream.
    fn push(&mut self, change: Change) -> Vec<Change>;

    /// Push a change originating from the child input of a join.
    /// In TS, join wires parent.setOutput → #pushParent and child.setOutput → #pushChild.
    /// Default: delegates to push() (correct for non-join operators).
    fn push_child(&mut self, change: Change) -> Vec<Change> {
        self.push(change)
    }

    /// Get the operator type name (for debugging/logging).
    fn op_type(&self) -> &'static str;
}
