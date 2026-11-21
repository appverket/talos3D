# Portable knowledge protocol

This crate owns the domain-neutral asset envelope shared by Talos3D and the
curator service. It has no modeling engine, credentials, transport, domain assets
or release authority. Consumers use `Asset::validate_envelope`, `asset_schema`
and `contract_manifest`; do not recreate these rules in tool descriptions.

The generated schema includes kind/path relationships, identifiers, required
metadata and portable-body restrictions. Runtime-specific payload decoders remain
in the app. Provenance, rights and independent release decisions remain in the
curator service. Passing an envelope or payload check establishes neither.

`InvalidEnvelope` returns JSON pointers, stable violation codes and protocol-owned
expected values. It never echoes rejected private content. The manifest exposes a
canonical schema fingerprint for compatibility checks before inference.

Run `cargo test -p talos3d-knowledge-protocol` for deterministic schema/decoder
conformance, including the historical incident-path versus asset-path mismatch.
These are protocol tests, not model qualification or evidence of a curator repair.
