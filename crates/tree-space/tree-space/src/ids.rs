//! Fixed-width persistent identities.

use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

macro_rules! fixed_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(pub [u8; 16]);

        impl $name {
            /// Creates a process-unique 16-byte value suitable for a new object.
            pub fn new() -> Self {
                let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let mut value = [0_u8; 16];
                value[..8].copy_from_slice(&counter.to_le_bytes());
                value[8..].copy_from_slice(&(!counter).to_le_bytes());
                Self(value)
            }

            /// Constructs the identity from its Arrow `FixedSizeBinary(16)` bytes.
            pub const fn from_bytes(value: [u8; 16]) -> Self {
                Self(value)
            }

            /// Returns the Arrow `FixedSizeBinary(16)` bytes.
            pub const fn as_bytes(self) -> [u8; 16] {
                self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                for byte in self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }
    };
}

fixed_id!(TypeId, "Stable identity of a domain or table type.");
fixed_id!(NodeId, "Stable identity of a domain-node instance.");
fixed_id!(TableId, "Stable identity of a physical table instance.");
fixed_id!(Digest, "A 16-byte canonical content digest.");

/// Fast path identity for a table path; the full path remains authoritative.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PathHash(pub u128);

impl PathHash {
    /// The deliberate degraded value used after a detected collision.
    pub const DEGRADED: Self = Self(0);
}

impl Display for PathHash {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:032x}", self.0)
    }
}

/// Version of a persisted table type schema.
pub type SchemaVersion = u32;
