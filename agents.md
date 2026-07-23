# Instructions for AI agents

This file defines standard procedures every AI agent must follow when working on Pubsplash.

## Keep the changelog up to date

`changelog.md` must be updated in the same change set as the code it describes.

- Every completed item gets its own bullet point.
- Entries go under the permanent `## Unreleased` major heading until a release is cut; each released version gets its own `## <version>` major heading.
- Under each major heading, place entries in exactly one of the three subheadings: `### Additions`, `### Fixes`, or `### Changes`.
- When a release is tagged, rename `Unreleased`'s content to the new version heading and recreate an empty `Unreleased` section above it.

## Keep the README up to date

`README.md` is user-facing documentation and must always reflect the current behavior of the app.

- When you add, remove, or change a feature, shortcut, setting, or requirement, update the corresponding README section in the same change set.
- The keyboard shortcut table must list every shortcut the app actually binds.
- The README is converted to HTML and shipped with releases, so keep it self-contained (no relative links into the source tree).

## Other conventions

- The app version shown in the About dialog comes from `Cargo.toml`; bump it as part of cutting a release.
- Configuration schema changes must remain backward compatible or handled by the corruption/defaults recovery path in `src/config.rs`; document new settings in the README.
