//! Entitlement resolution stub.
//!
//! An `EntitlementHook` on a `PackManifest` is an opaque reference to an
//! operator-defined commercial policy. The substrate carries the reference
//! but never resolves it itself — resolution belongs in the operator layer
//! (Phase-1 gated-binary story per the commercial plan).
//!
//! This module defines the `EntitlementResolver` trait and the default
//! `AllowAllEntitlements` implementation that is used in core. Real
//! implementations live in operator plugins and are injected at runtime.

use super::identity::PackId;
use super::pack::EntitlementHook;

/// The identity of the actor requesting access. Opaque string in Phase 1;
/// will carry more structure once the operator auth layer exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Actor(pub String);

impl Actor {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Outcome of an entitlement check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entitlement {
    /// The actor is entitled to use the pack.
    Allowed,
    /// The actor is not entitled. `reason` is a human-readable string
    /// suitable for surfacing in error messages.
    Denied { reason: String },
}

impl Entitlement {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    pub fn denied(reason: impl Into<String>) -> Self {
        Self::Denied {
            reason: reason.into(),
        }
    }
}

/// Typed error from the entitlement resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntitlementError {
    /// The hook reference format was not recognized by this resolver.
    UnrecognizedHook { hook: EntitlementHook },
    /// The resolver encountered an internal error (network, crypto, etc.)
    /// and could not determine entitlement. The load should be rejected
    /// unless the caller explicitly opts in to fail-open.
    Internal { message: String },
}

impl std::fmt::Display for EntitlementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnrecognizedHook { hook } => {
                write!(f, "unrecognized entitlement hook '{}'", hook.as_str())
            }
            Self::Internal { message } => {
                write!(f, "entitlement resolver internal error: {message}")
            }
        }
    }
}

impl std::error::Error for EntitlementError {}

/// Pluggable entitlement resolver. The core ships `AllowAllEntitlements`;
/// operator plugins register a replacement before any gated pack is loaded.
///
/// The trait is object-safe so callers can hold a `Box<dyn EntitlementResolver>`.
pub trait EntitlementResolver: Send + Sync {
    /// Check whether `actor` is entitled to use pack `pack_id` given the
    /// `hook` stored on the manifest. `hook` is `None` for open packs;
    /// open packs should always return `Entitlement::Allowed`.
    fn is_entitled(
        &self,
        pack_id: &PackId,
        actor: &Actor,
        hook: Option<&EntitlementHook>,
    ) -> Result<Entitlement, EntitlementError>;
}

/// Default implementation: allows all packs regardless of entitlement hook.
/// Used in core until an operator plugin overrides it.
pub struct AllowAllEntitlements;

impl EntitlementResolver for AllowAllEntitlements {
    fn is_entitled(
        &self,
        _pack_id: &PackId,
        _actor: &Actor,
        _hook: Option<&EntitlementHook>,
    ) -> Result<Entitlement, EntitlementError> {
        Ok(Entitlement::Allowed)
    }
}

/// Stub that always denies. Useful in tests to verify that `load_pack`
/// actually consults the resolver.
pub struct AlwaysDenyEntitlements {
    pub reason: String,
}

impl AlwaysDenyEntitlements {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl EntitlementResolver for AlwaysDenyEntitlements {
    fn is_entitled(
        &self,
        _pack_id: &PackId,
        _actor: &Actor,
        _hook: Option<&EntitlementHook>,
    ) -> Result<Entitlement, EntitlementError> {
        Ok(Entitlement::denied(self.reason.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::identity::PackId;

    fn pack() -> PackId {
        PackId::new("test_pack")
    }

    fn actor() -> Actor {
        Actor::new("user:test")
    }

    fn hook() -> EntitlementHook {
        EntitlementHook::new("appverket/paddle/SKU-SE-01")
    }

    #[test]
    fn allow_all_permits_regardless_of_hook() {
        let resolver = AllowAllEntitlements;
        assert!(resolver
            .is_entitled(&pack(), &actor(), Some(&hook()))
            .unwrap()
            .is_allowed());
        assert!(resolver
            .is_entitled(&pack(), &actor(), None)
            .unwrap()
            .is_allowed());
    }

    #[test]
    fn always_deny_rejects_with_reason() {
        let resolver = AlwaysDenyEntitlements::new("not subscribed");
        let result = resolver
            .is_entitled(&pack(), &actor(), Some(&hook()))
            .unwrap();
        assert!(!result.is_allowed());
        match result {
            Entitlement::Denied { reason } => assert_eq!(reason, "not subscribed"),
            Entitlement::Allowed => panic!("expected Denied"),
        }
    }

    #[test]
    fn entitlement_error_display_is_human_readable() {
        let e = EntitlementError::UnrecognizedHook { hook: hook() };
        assert!(e.to_string().contains("appverket/paddle/SKU-SE-01"));
    }
}
