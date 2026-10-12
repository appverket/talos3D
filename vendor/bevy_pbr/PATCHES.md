# Local Bevy PBR repairs

Source: crates.io `bevy_pbr` 0.19.1, registry SHA-256
`244ae7d618b51a59c913c36b0564a295cd85ea91a5e777b542bb7bdb846a23d6`.
Upstream: https://github.com/bevyengine/bevy/tree/v0.19.1/crates/bevy_pbr
Licenses: MIT OR Apache-2.0; original license files retained.

The package source differs in `src/render/light.rs` and the StandardMaterial
Rust/WGSL clipping paths listed below. The normalized registry Cargo manifest is retained so dependencies
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


## StandardMaterial world-space clipping

The upstream StandardMaterial has no runtime half-space clipping input. Talos3D
previously hid only whole mesh bounds, leaving crossing walls intact in section
views. This local extension adds up to eight world-space plane uniforms and
fragment discard in forward/deferred and depth/shadow passes. Clipped opaque
materials select a mask-capable pipeline without mutating their authored alpha
mode. Planes are derived rendering state and are not added to the native project
material schema. Talos3D updates only changed material assets and plane sets.

Affected source: `src/pbr_material.rs`, `src/render/pbr_types.wgsl`,
`src/render/pbr_fragment.wgsl`, `src/render/pbr_prepass_functions.wgsl`.
The fixed uniform array is compatible with WebGL2 uniform buffers. Talos3D fails
closed and reports an error if more than eight planes are active; it never
silently renders a subset. This patch does not generate section caps.

Proposed upstream shape: opt-in StandardMaterial world-space clip planes shared
by color, prepass and shadow fragments, with explicit bounded backend capacity.
`standard-material-clipping.patch` records the proposed implementation; no upstream PR has
been submitted. Consume an upstream equivalent before removing these fields.
