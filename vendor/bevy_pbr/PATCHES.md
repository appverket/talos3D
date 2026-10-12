# Temporary Bevy directional-light priority repair

Source: crates.io `bevy_pbr` 0.19.1, registry SHA-256
`244ae7d618b51a59c913c36b0564a295cd85ea91a5e777b542bb7bdb846a23d6`.
Upstream: https://github.com/bevyengine/bevy/tree/v0.19.1/crates/bevy_pbr
Licenses: MIT OR Apache-2.0; original license files retained.

The package source is unchanged except `src/render/light.rs` and this patch
record. The normalized registry Cargo manifest is retained so dependencies
still resolve to the released engine line. Cargo-generated cache markers,
registry provenance JSON and the crate-local lockfile are excluded.

`directional-light-priority.patch` is the proposed upstream change; it has not
been submitted upstream. The comparator sorted bools ascending, retaining
unshadowed fill before a shadow-casting key despite the documented priority.
On WebGL's one-directional-light backend this discarded the scene's sun.
Shadow/volumetric allocation counts also used the unsorted ECS query prefix;
they now use the same sorted, capacity-limited list as GPU light extraction.

Regression tests exercise a fill preceding a shadowed key with capacity one,
volumetric/shadow precedence, stable identity order, and retained-list counts.
The actual runtime still requires browser screenshot verification. This does
not raise WebGL's light limit and does not alter authored scene light values.

Removal: consume a compatible released Bevy version containing this fix (or a
reviewed upstream revision until released), delete this vendor directory and
both application workspace patches, and re-run backend rendering tests.
