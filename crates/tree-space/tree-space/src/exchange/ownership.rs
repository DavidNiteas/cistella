//! Ownership determination (protocol `交换协议/01-目标与设计.md` §3.1): only
//! `Holder::Own` may write.
//!
//! This is the single-writer gate the write path consults (E-2, `02-施工路线图.md`
//! §6). Owner direct writes (`TbLibrary::commit`/`commit_pruned`) and proxy
//! writes (`crate::exchange::proxy::handle_proxy_request`) both serialize
//! through the owner's `metadata_lock`, so all writes remain logically
//! single-writer (01 §3.2).

use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::exchange::{ExchangeKey, RuntimeTable};

/// The single-writer gate (01 §3.1): returns `Ok` iff the runtime table marks
/// `key` as `Holder::Own`; any mapped holder or an absent block entry is
/// `Err(ErrorCode::OwnershipDenied)`.
///
/// An absent block entry is denied too — "not locally owned" is the unified
/// write-gate semantics (`02-施工路线图.md` §6.6): the caller may not write a
/// block it does not have a runtime entry for.
pub fn require_owner(table: &RuntimeTable, key: ExchangeKey) -> Result<()> {
    let holder = match key {
        ExchangeKey::Tree => &table.tree_entry().holder,
        ExchangeKey::Block(id) => match table.block_entry(id) {
            Some(entry) => &entry.holder,
            None => return Err(ownership_denied("block is not locally owned")),
        },
    };
    if holder.is_owner() {
        Ok(())
    } else {
        Err(ownership_denied("only the owner may write"))
    }
}

fn ownership_denied(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::OwnershipDenied, message)
}
