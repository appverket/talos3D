# Contributing

Talos3D is maintained on a time-permits basis. Contributions are welcome, but
review is discretionary and there is no guarantee that a pull request will be
merged or even reviewed quickly.

## Scope

Talos3D is being built as an AI-first 3D platform with a public extension
surface. Contributions should improve one or more of:

- authored model clarity
- command and history correctness
- capability pluggability
- AI/model API completeness
- interaction quality
- documentation quality

Contributions are most useful when they strengthen the public platform surface
or reduce maintenance burden. Changes that add long-term ownership cost without
clear platform value are less likely to land.

## Read First

Start with:

1. [README.md](./README.md)
2. [AGENTS.md](./AGENTS.md)
3. [MCP Model API](./docs/MCP_MODEL_API.md)
4. [Developer Onboarding](./docs/DEVELOPER_ONBOARDING.md)
5. [Core Principles](./docs/CORE_PRINCIPLES.md)
6. [Platform Architecture](./docs/PLATFORM_ARCHITECTURE.md)
7. [Extension Architecture](./docs/EXTENSION_ARCHITECTURE.md)
8. [Domain Model](./docs/DOMAIN_MODEL.md)

Also read:

1. [GOVERNANCE.md](./GOVERNANCE.md)
2. [SUPPORT.md](./SUPPORT.md)
3. [SECURITY.md](./SECURITY.md)

## Before You Start

- For substantial changes, open an issue first so scope and architecture fit
  can be checked before you spend time on a patch.
- Small documentation fixes and targeted bug fixes are easiest to review.
- The maintainer may decline contributions that create too much long-term
  maintenance burden, even if the code is technically sound.
- If a feature is valuable but too opinionated or too costly for the main repo,
  a fork or separate capability crate may be the right home for it.

## Development Commands

```bash
cargo check
cargo clippy -- -W clippy::all
cargo test
cargo run
cargo run --features model-api
```

## Architectural Rules

- The authored model is the source of truth.
- Meshes and previews are derived artifacts.
- User-facing edits go through commands and history.
- Capabilities are the primary extension unit.
- Domain behavior is layered on top of modeling behavior.
- AI must be able to inspect and act on the same platform concepts humans use.

## Capability Contributions

A new feature should usually land as one of:

- a core-platform improvement
- a modeling capability
- a domain capability
- a workbench-level composition/default

When in doubt, prefer explicit capability boundaries over hidden cross-module
coupling.

The architectural code in this repository is a reference extension. New domain
packages should be able to follow the same pattern whether they are open-source,
private, or commercial.

## Submission Terms

By submitting a contribution, you agree that your contribution is licensed
under the repository license terms.

Commits must be signed off with the Developer Certificate of Origin (DCO):

```text
Signed-off-by: Your Name <you@example.com>
```

The easiest way to do this is:

```bash
git commit -s
```

Use your real name or the name you normally use for legal attestations. The
full DCO text is available at <https://developercertificate.org/>.

## Review Reality

- Review timing is unpredictable.
- Silence does not imply rejection, but it also does not imply future action.
- A pull request may be closed simply because the maintainer does not want to
  own that change.

If you need certainty, maintain your change in a fork or separate extension.

## Documentation

Public documentation is Markdown-first. The repository includes a site
configuration so the docs website can be generated from the same Markdown used
for repository onboarding.

Before merging a change that affects terminology or architecture, update the
relevant docs in `docs/` and the root `README.md`.

The `private/` directory is archival material for the maintainer and is not
part of the intended public repository contents.
