//! Tree plugins (`01-目标与设计.md` §4-1): symmetric to block plugins, but for
//! the canonical tree object (encode/decode/tree_id/address).
//!
//! [`Arrow55TreePlugin`] wraps `tree::codec` (encode/decode/tree_id) with
//! `DISK = ArrowIpc` and memory layout `Arrow55`. PL-2 M2 switches the tree
//! identity formula to the semantic walk (01 §4-4): `tree_id =
//! hash(tb-tree, [semantic_tree(image)])` — the boot block's
//! `tree_mem_version` / `tree_disk_version` strings (`01 §4-3`) must match the
//! routed tree plugin's `MEMORY` / `DISK` declarations.

use crate::error::Result;
use crate::ids::Digest;
use crate::layout::tb::tree_blob_address;
use crate::plugin::semantic::semantic_tree;
use crate::plugin::version::{DiskLayout, MemLayout};
use crate::tree::codec::TreeImage;

/// A tree plugin: declares the memory/disk layout pair it serves and wraps the
/// canonical tree object operations (`01 §4-1`).
///
/// Stable Rust cannot make a trait with associated constants object-safe
/// (E0038), so the `MEMORY` / `DISK` declarations of the spec (`02 §5.4`) are
/// presented as the equivalent required query methods
/// [`TreePlugin::memory`] / [`TreePlugin::disk`] (deviation noted in the PL-1
/// S2 report).
pub trait TreePlugin: Send + Sync {
    /// The in-memory tree layout version this plugin serves (M1: [`MemLayout::Arrow55`]).
    fn memory(&self) -> MemLayout;
    /// The declared on-disk tree layout version (M1: [`DiskLayout::ArrowIpc`]).
    fn disk(&self) -> DiskLayout;
    /// Computes the canonical tree identity of encoded tree bytes.
    ///
    /// PL-2 M2 implementation = `tree::codec::tree_id` (the semantic formula
    /// `hash(tb-tree, [semantic_tree(image)])`, 01 §4-4); the M1 byte formula
    /// was the PL-1 default (zero golden change at M1).
    fn tree_id(&self, bytes: &[u8]) -> Digest {
        Digest::from_bytes(crate::tree::codec::tree_id(bytes).as_bytes())
    }
    /// Returns the semantic walk fingerprint of a tree image
    /// (`01 §4-1 [M2]`): the fingerprint component of the tree identity
    /// formula — `tree_id = hash(tb-tree, [semantic(image)])`.
    fn semantic(&self, image: &TreeImage) -> Vec<u8> {
        semantic_tree(image)
    }
    /// Encodes an in-memory tree image into canonical disk bytes.
    fn encode(&self, image: &TreeImage) -> Result<Vec<u8>> {
        crate::tree::codec::encode(image)
    }
    /// Decodes canonical disk bytes back into the tree image.
    fn decode(&self, bytes: &[u8]) -> Result<TreeImage> {
        crate::tree::codec::decode(bytes)
    }
    /// Computes the storage address of canonical tree bytes
    /// (`tree_blob_address`, immutable hash formula).
    fn address(&self, bytes: &[u8]) -> Digest {
        tree_blob_address(bytes)
    }
}

/// The M1 tree plugin: wraps `tree::codec`'s encode/decode/tree_id
/// (`DISK = ArrowIpc`, memory layout `Arrow55`).
pub struct Arrow55TreePlugin;

impl TreePlugin for Arrow55TreePlugin {
    fn memory(&self) -> MemLayout {
        MemLayout::Arrow55
    }

    fn disk(&self) -> DiskLayout {
        DiskLayout::ArrowIpc
    }
}
