# Changelog

## Unreleased

- The public Core app joins the root Cargo workspace and lockfile. Workspace
  checks now include its entrypoint and MCP feature configuration.
- Architecture's offline app and domain integration tests move to its own repo.
  Generic curation contract tests join the public workspace.
- Shared component-family operations and component-structure advice are
  available through Core. Capabilities contribute discovery guidance, aliases,
  host-geometry/setting-out policies, support vocabulary and material presets.
- Domain snapshots supply host-axis, creation-label and restore-order behavior.
  The Definition browser uses capability-owned `HostedSelectionContext`; its
  former wall fields are renamed to host fields. Browser/API face anchors and
  orientation share Core helpers. Legacy `host_mode: wall` remains an alias.
  Core no longer inspects serialized Wall payloads or dispatches on building
  element classes. Architecture retains its model identifiers and command ids.
- Orthographic peak analysis scales with generic objects; Architecture supplies
  its existing building-scale analysis thresholds through a policy resource.
- Manufacturer-specific cedar appearance assets move to Architecture. Generic
  material defaults support capability presets across document replacement.
- Public setup documentation, package metadata, issue/PR templates, repository
  checks and the strict documentation build are refreshed.

This development log records current changes; it does not imply a released
version or a stable plugin ABI.
