//! Tree leaf position compatibility exports.
//!
//! TB keeps the word "leaf" for a tree position. Payload objects are blocks
//! exported from [`crate::block`], and positions use [`crate::tree::Slot`].

pub use crate::block::{Block, BlockDesc, BlockKind, RefId, Table as ArrowTable, Value};
