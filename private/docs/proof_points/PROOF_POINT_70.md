# Proof Point 70: Authored MaterialAssignment And Layer Sets

**Status**: Planned  
**Date**: 2026-04-20

## Goal

Promote material assignment from a single render-material id into authored
binding state that can represent direct bindings and ordered layer sets.

## Scope

- replace the single-field `MaterialAssignment` component with typed bindings
- ship `Single` and `LayerSet`
- keep runtime rendering behavior for simple render-material assignment intact
- defer `ConstituentSet`

## Why It Matters

Materials and layer sets belong in authored object state, not in evaluator
parameter schemas. This proof point establishes that boundary in code.

## Expected Verification

- serialization round-trip tests for `Single` and `LayerSet`
- existing apply/remove-material flows continue to work for simple render
  bindings
- section-fill / material-application systems tolerate the richer component
  shape
