use crate::types::{Change, FetchRequest, Node};

pub trait Operator: Send {
    /// Fetch data matching the request. Returns nodes in sort order.
    fn fetch(&mut self, req: &FetchRequest) -> Vec<Node>;

    /// Push an incremental change through this operator.
    /// Returns output changes to propagate downstream.
    fn push(&mut self, change: Change) -> Vec<Change>;

    /// Get the operator type name (for debugging/logging).
    fn op_type(&self) -> &'static str;
}
