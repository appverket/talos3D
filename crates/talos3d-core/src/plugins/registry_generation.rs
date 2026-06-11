//! Process-unique change stamps for registry-backed UI caches.
//!
//! Browser windows (Definitions, Materials) cache derived entry lists instead
//! of rebuilding them every egui frame. A cache keyed on a plain per-registry
//! counter would go stale when a whole registry resource is replaced (project
//! load deserializes a fresh registry whose counter would restart at the same
//! values), so stamps are drawn from one process-wide monotonic sequence:
//! every mutation *and* every newly constructed registry instance gets a
//! value never handed out before. Registries hold their stamp in a
//! `#[serde(skip)]` field, so deserialization goes through `Default` and a
//! loaded registry always carries a brand-new stamp. Two registry states with
//! equal stamps are therefore guaranteed identical within a process.

use std::sync::atomic::{AtomicU64, Ordering};

/// Opaque change stamp. Compare with `==` to decide whether a cached view of
/// a registry is still current; the numeric value carries no other meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryGeneration(u64);

impl RegistryGeneration {
    fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// Record that the owning registry mutated.
    pub(crate) fn bump(&mut self) {
        *self = Self::fresh();
    }
}

/// A fresh default means a default-constructed (or deserialized, via
/// `#[serde(skip)]`) stamp never accidentally matches an existing one.
impl Default for RegistryGeneration {
    fn default() -> Self {
        Self::fresh()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_instances_never_collide() {
        let a = RegistryGeneration::default();
        let b = RegistryGeneration::default();
        assert_ne!(a, b);
    }

    #[test]
    fn bump_changes_the_stamp() {
        let mut stamp = RegistryGeneration::default();
        let before = stamp;
        stamp.bump();
        assert_ne!(before, stamp);
    }
}
