# Local Bevy PBR repairs

Source: crates.io `bevy_pbr` 0.20.1, registry SHA-256
`1138d6ed9bbcc43a4d83878062869c20d4292f1112b177acfc1a20cf6ae14f3e`.
Upstream: https://github.com/bevyengine/bevy/tree/v0.20.1/crates/bevy_pbr
Licenses: MIT OR Apache-2.0; original license files retained.

The package source differs in `src/render/light.rs` and the StandardMaterial
Rust/WESL clipping paths listed below. Both repairs were rebased from 0.19.1 through 0.20.0 to 0.20.1;
0.20.1 still sorts directional lights with the ascending bool comparator. The normalized registry Cargo manifest is retained so dependencies
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

Affected source: `src/pbr_material.rs`, `src/render/pbr_types.wesl`,
`src/render/pbr_fragment.wesl`, `src/render/pbr_prepass_functions.wesl`.
Bevy 0.20 moved its shaders to WESL; the clip loop uses `@if(BINDLESS)`
selection and runs in the prepass inside the `MAY_DISCARD` path that clipped
materials select through `AlphaMode::Mask`.
The fixed uniform array is compatible with WebGL2 uniform buffers. Talos3D fails
closed and reports an error if more than eight planes are active; it never
silently renders a subset. This patch does not generate section caps.

Proposed upstream shape: opt-in StandardMaterial world-space clip planes shared
by color, prepass and shadow fragments, with explicit bounded backend capacity.
`standard-material-clipping.patch` records the proposed implementation; no upstream PR has
been submitted. Consume an upstream equivalent before removing these fields.

## 0.20.1 rebase audit

The 0.20.0-to-0.20.1 upstream diff changes volumetric-fog near-plane handling
inside PBR. Those upstream Rust/WESL changes are included verbatim. Neither
directional-light prioritization nor StandardMaterial clip-plane support is
present in the released source, so both small prepared upstream patches remain
necessary. Their source files have no conflicting upstream edits; the original
patches apply cleanly and retain their regression coverage.

This remains a minimal component vendor, not a fork of the entire renderer.
The desired upstream interfaces and removal conditions above still apply. The
priority correction can be proposed independently; clip planes need upstream
API review for bounded backend capacity and all render passes before submission.
The prepared patches are retained as reviewable upstream proposals, without
claiming an upstream PR has been opened or accepted.
