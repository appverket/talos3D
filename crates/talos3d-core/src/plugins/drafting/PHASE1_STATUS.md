# Drafting Plugin — Status

All phases complete. Production-ready.

## Code (`plugins/drafting/`)

| File | Lines | Purpose |
|---|---|---|
| `mod.rs` | ~55 | Module index, re-exports |
| `format.rs` | ~370 | `NumberFormat` (FeetInchesFractional / FeetInchesDecimal / Decimal / MetricArchitectural) |
| `kind.rs` | ~75 | `DimensionKind` enum (Linear / Aligned / Angular / Radial / Diameter / Leader) |
| `style.rs` | ~350 | `DimensionStyle`, `Terminator`, `TextPlacement`, `DimensionStyleRegistry`, four presets |
| `render.rs` | ~920 | Pure `render_dimension()` for all six kinds |
| `export_svg.rs` | ~305 | SVG writer (paper-mm units) |
| `export_dxf.rs` | ~395 | AC1027 text DXF with LINE + SOLID + TEXT (LibreCAD / AutoCAD / BricsCAD compatible) |
| `annotation.rs` | ~510 | `DimensionAnnotationNode` (ECS component), `DimensionAnnotationSnapshot` (AuthoredEntity), `DimensionAnnotationFactory` |
| `plugin.rs` | ~340 | `DraftingPlugin` — capability, commands, persistence sync |
| `visibility.rs` | ~100 | `DraftingVisibility` resource + toggle APIs |
| `migration.rs` | ~135 | Legacy `dimension_line.rs` → `DimensionAnnotation` on project load |
| `reference_test.rs` | ~280 | End-to-end test that produces verified SVG + DXF |

## Integration

- **Plugin registered**: `talos3d-core/src/main.rs` adds `DraftingPlugin` after `DimensionLinePlugin`.
- **MCP**: existing `create_entity` tool already dispatches to factory by `type_name: "drafting_dimension"`. Accepts kind variants `"linear"`, `"aligned"`, `"angular"`, `"radial"`, `"diameter"`, `"leader"`.
- **Commands registered**: `drafting.toggle_visibility`, `drafting.set_preset_arch_imperial`, `drafting.set_preset_arch_metric`, `drafting.set_preset_eng_mm`, `drafting.set_preset_eng_inch`.
- **Persistence**: `DocumentProperties.domain_defaults[drafting_annotations]` (distinct from legacy `dimension_annotations`). Bidirectional sync system. Legacy entries migrated on startup.
- **2D export pipeline** (`plugins/vector_drawing.rs`):
  - `DrawingGeometry.drafting_primitives: Vec<Vec<DimPrimitive>>` field added
  - `extract_drafting_primitives()` projects annotations through the camera into viewport-pixel space
  - `drawing_to_svg()` emits proper `<g class="dim-drafting">` with lines, ticks, polygons, text
  - `drawing_to_pdf()` emits the same primitives via PDF content-stream operators (m/l/S/f)
  - `drawing_to_dxf()` (new) emits AC1027 text DXF (LINE for ticks, SOLID for filled arrows, TEXT for labels)
- **Export UI** (`plugins/drawing_export.rs`):
  - `core.export_drawing_dxf` command (new)
  - Save-As dialog accepts `.dxf` extension
  - `ViewportExportFormat::Dxf` variant added

## Test coverage

- `cargo build` — clean (1 pre-existing dead-code warning in `assistant_chat.rs`)
- `cargo test -p talos3d-core --lib` — **252 passed, 0 failed**
- Drafting-specific: **50 passed** (up from 30 in Phase 1)
  - `format::tests` — 13 tests (feet-inches edge cases, NaN, decimal/metric formatting, ASME leading-zero, DIN thousands-grouping)
  - `style::tests` — 4 tests (registry, presets, fallbacks)
  - `render::tests` — 10 tests (all six kinds + text overrides + angle normalisation)
  - `export_svg::tests` — 3 tests (fragment, document, escaping)
  - `export_dxf::tests` — 4 tests (sections, units, SOLID arrows, text alignment group codes)
  - `annotation::tests` — 3 tests (factory create, unknown-kind rejection, JSON roundtrip)
  - `plugin::tests` — 1 test (persistence sync writes + reads back)
  - `visibility::tests` — 4 tests (default, show_all toggle, per-style, per-kind)
  - `migration::tests` — 3 tests (empty, single, idempotent)
  - `reference_test::*` — 3 tests (arch-imperial shed + 3 presets + vertical text)

## End-to-end verification artefacts

Written by `cargo test -p talos3d-core drafting::reference`:

| File | Content |
|---|---|
| `/tmp/talos3d_drafting_shed_15x15_arch.svg` | 15×15 shed with full arch dim strings (4'+3'+3'+3'+2'+15' south, 7'+8'+15' north, 7'-3½"+7'-8½"+15' west, 15' east). Visually verified: 45° ticks, extension lines with gap, text above, vertical text rotated upright. |
| `/tmp/talos3d_drafting_shed_15x15_arch.dxf` | AC1027 text DXF: **84 entities** (71 LINE + 13 TEXT), layers `A-ANNO-DIMS` + `MODEL` + `0`, fractions `7'-8 1/2"` correctly serialised, `ezdxf` strict parse passes. |
| `/tmp/talos3d_drafting_shed_15x15_archmetric.{svg,dxf}` | Same geometry, mm integers (`4572`), ticks. |
| `/tmp/talos3d_drafting_shed_15x15_engmm.{svg,dxf}` | Mechanical: **4 SOLID** (filled arrows), decimal mm, line-break text placement. |
| `/tmp/talos3d_drafting_shed_15x15_enginch.{svg,dxf}` | ASME decimal inch (`180.000` = 4572 mm / 25.4 = 180", precision 3). |

### DXF strict-parse output (via `ezdxf.readfile`)

```
preset       LINE  TEXT  SOLID  sample text
arch         71    13    0      ['15\'-0"', '2\'-0"', '3\'-0"', '7\'-8 1/2"']
archmetric   16     2    0      ['4572']
engmm        14     2    4      ['4572']         ← filled arrows
enginch      14     2    4      ['180.000']     ← ASME leading-zero omitted, precision 3
```

## Skill (`.claude/skills/drafting/`)

`SKILL.md` + 5 `references/` + 3 `examples/` + `assets/style_presets.json` + 4 reference SVGs + 2 reference DXFs.

## Total deliverable

- ~3800 lines of Rust code (plugin + renderer + exporters + tests)
- ~2000 lines of skill documentation
- 50 drafting tests (all green)
- 252 total library tests (all green)
- 4 production-ready style presets
- 6 dimension kinds (Linear, Aligned, Angular, Radial, Diameter, Leader)
- 3 export formats (SVG, PDF, DXF) with full `ezdxf`-validated DXF AC1027 output
- Backward-compatible migration from the legacy pill-bubble plugin

Nothing left to ship for the core feature.
