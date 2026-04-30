#![deny(clippy::all)]

pub mod filter;
pub mod join;
pub mod storage;
pub mod take_state;
pub mod exists;
pub mod types;
pub mod operator;
pub mod filter_op;
pub mod skip_op;
pub mod cap_op;
pub mod join_op;
pub mod take_op;
pub mod exists_op;
pub mod or_exists_op;
pub mod flipped_join_op;
pub mod union_fan_in_op;
pub mod union_fan_out_op;
pub mod pipeline;
