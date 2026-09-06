//! xpath → `RefId` flat index with collision degradation (N-8).
//!
//! The index maps an [`XPath`] to the [`RefId`] of the leaf it addresses in a
//! composition. It is a *lookup accelerator*, never an authority: the physical
//! payload handle remains the `RefId` (content hash), and the canonical xpath
//! bytes are the key input. The two-hash rule is preserved — `XPathHash` is
//! only an index key.

use crate::block::RefId;
use crate::hash::canonical_digest;
use crate::xpath::{Step, XPath};
use std::collections::BTreeMap;

/// The 16-byte flat index key for an xpath.
pub type XPathHash = [u8; 16];

/// Stable, collision-safe canonical bytes of an xpath.
///
/// Deliberately *not* the `Display` form: the injection encoding length-prefixes
/// every step so distinct addresses can never canonicalize to the same bytes:
///
/// - `Field(name)` → `0x46 ++ u64le(name.len) ++ name`
/// - `Index(i)`    → `0x49 ++ u64le(i)`
pub fn canonical_xpath_bytes(xpath: &XPath) -> Vec<u8> {
    let mut out = Vec::new();
    for step in xpath.steps() {
        match step {
            Step::Field(name) => {
                out.push(0x46);
                out.extend_from_slice(&(name.len() as u64).to_le_bytes());
                out.extend_from_slice(name.as_bytes());
            }
            Step::Index(index) => {
                out.push(0x49);
                out.extend_from_slice(&(*index as u64).to_le_bytes());
            }
        }
    }
    out
}

/// Hashes the canonical bytes of an xpath to its flat index key.
pub fn xpath_hash(xpath: &XPath) -> XPathHash {
    let digest = canonical_digest(b"xpath", [canonical_xpath_bytes(xpath)]);
    digest.as_bytes()
}

/// Decodes the canonical injection-encoded bytes back into an [`XPath`].
///
/// This is the exact inverse of [`canonical_xpath_bytes`] and is used to
/// reconstruct `XPath` values from persisted reference tables (the tree-space
/// parquet-dir TB surface). The encoding tags are `0x46` for variable-length
/// content (u64 length prefix + bytes) and `0x49` for a fixed-width u64 index
/// (no length prefix); the former `0x4b` Key tag was removed by the xpath
/// addressing normalization and is rejected. Any other tag or a truncated
/// payload is an error.
pub fn xpath_from_canonical_bytes(bytes: &[u8]) -> crate::error::Result<XPath> {
    let mut steps = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = bytes[offset];
        offset += 1;
        match tag {
            0x46 => {
                let (name, next) = read_len_bytes(bytes, offset)?;
                let name = std::str::from_utf8(name)
                    .map_err(|_| mp("canonical field bytes are not UTF-8"))?;
                if name.is_empty() {
                    return Err(mp("canonical Field step has an empty name"));
                }
                steps.push(Step::Field(name.to_owned()));
                offset = next;
            }
            0x49 => {
                let end = offset
                    .checked_add(8)
                    .ok_or_else(|| mp("canonical Index length overflows"))?;
                let value = bytes
                    .get(offset..end)
                    .ok_or_else(|| mp("truncated canonical Index step"))?;
                let index = u64::from_le_bytes(value.try_into().expect("checked length"));
                steps.push(Step::Index(
                    usize::try_from(index)
                        .map_err(|_| mp("canonical Index value exceeds usize"))?,
                ));
                offset = end;
            }
            0x4b => {
                return Err(mp(
                    "canonical xpath Key step tag 0x4b is no longer supported",
                ));
            }
            _ => return Err(mp("unknown xpath canonical step tag")),
        }
    }
    Ok(XPath::from_steps(steps))
}

/// Reads a u64 length prefix followed by exactly `len` bytes.
fn read_len_bytes(bytes: &[u8], offset: usize) -> crate::error::Result<(&[u8], usize)> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| mp("canonical xpath length overflows"))?;
    let len = u64::from_le_bytes(
        bytes
            .get(offset..end)
            .ok_or_else(|| mp("truncated canonical xpath length"))?
            .try_into()
            .expect("checked length"),
    ) as usize;
    let start = end;
    let final_offset = start
        .checked_add(len)
        .ok_or_else(|| mp("canonical xpath payload overflows"))?;
    let payload = bytes
        .get(start..final_offset)
        .ok_or_else(|| mp("truncated canonical xpath payload"))?;
    Ok((payload, final_offset))
}

fn mp(message: &str) -> crate::error::TreeSpaceError {
    crate::error::TreeSpaceError::new(crate::error::ErrorCode::PayloadMalformed, message)
}

/// A flat `xpath → RefId` index.
///
/// Internally a `BTreeMap<XPathHash, Vec<(XPath, RefId)>>`. The secondary
/// `XPath` comparison is the collision fuse: equal hashes from unequal paths
/// resolve by full canonical equality, never by hash alone.
#[derive(Clone, Debug, Default)]
pub struct XPathIndex {
    map: BTreeMap<XPathHash, Vec<(XPath, RefId)>>,
}

impl XPathIndex {
    /// Creates an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a mapping, appending to the collision chain when the hash is
    /// already taken by a *different* xpath.
    pub fn insert(&mut self, xpath: &XPath, id: RefId) {
        let hash = xpath_hash(xpath);
        self.map.entry(hash).or_default().push((xpath.clone(), id));
    }

    /// Looks up an `XPath`, confirming by full canonical equality.
    pub fn get(&self, xpath: &XPath) -> Option<&RefId> {
        let hash = xpath_hash(xpath);
        self.map
            .get(&hash)?
            .iter()
            .find(|(stored, _)| stored == xpath)
            .map(|(_, id)| id)
    }

    /// Removes one exact mapping, returning the referenced id if present.
    pub fn remove(&mut self, xpath: &XPath) -> Option<RefId> {
        let hash = xpath_hash(xpath);
        let chain = self.map.get_mut(&hash)?;
        let position = chain.iter().position(|(stored, _)| stored == xpath)?;
        let (_, id) = chain.swap_remove(position);
        if chain.is_empty() {
            self.map.remove(&hash);
        }
        Some(id)
    }

    /// Iterates over all stored `(xpath, id)` mappings.
    pub fn iter(&self) -> impl Iterator<Item = (&XPath, &RefId)> {
        self.map.values().flatten().map(|(xpath, id)| (xpath, id))
    }

    /// count_doc
    ///
    /// Debug/test probe that inserts a mapping under an explicitly supplied
    /// hash, used to exercise the collision-degradation path without relying
    /// on a (statistically impossible) real xxh3-128 collision.
    #[doc(hidden)]
    pub fn insert_debug_with_hash(&mut self, xpath: XPath, hash: XPathHash, id: RefId) {
        self.map.entry(hash).or_default().push((xpath, id));
    }

    /// Test probe returning the internal hash bucket size for the given hash.
    #[doc(hidden)]
    pub fn debug_bucket_len(&self, hash: &XPathHash) -> usize {
        self.map.get(hash).map_or(0, Vec::len)
    }

    /// Test probe that looks a mapping up inside an explicitly given bucket,
    /// exercising the collision-chain verification without a real hash clash.
    #[doc(hidden)]
    pub fn get_debug_with_hash(&self, xpath: &XPath, hash: &XPathHash) -> Option<&RefId> {
        self.map
            .get(hash)?
            .iter()
            .find(|(stored, _)| stored == xpath)
            .map(|(_, id)| id)
    }
}
