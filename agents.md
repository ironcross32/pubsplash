# Instructions for AI agents

This file defines standard procedures every AI agent must follow when working on Pubsplash.

## Keep the changelog up to date

`changelog.md` must be updated in the same change set as the code it describes.

- Every completed item gets its own bullet point.
- Keep entries concise and to the point; don't include **why** something was done, just **that** it was done
- Entries go under the permanent `## Unreleased` major heading until a release is cut; each released version gets its own `## <version>` major heading.
- Under each major heading, place entries in exactly one of the three subheadings: `### Additions`, `### Fixes`, or `### Changes`.
- When a release is tagged, rename `Unreleased`'s content to the new version heading and recreate an empty `Unreleased` section above it.

## Documentation

Do not make edits, additions, or modifications to the README unless instructed to do so. Instead, place proposed changes into a file called proposed_doc.md which does not get checked into the repo. If you've written to this document before in the same session, append to it, otherwise, overwrite its contents.

## Coding Strategy

- Whenever a decision is reached to accomplish a task by polling, determine if the same task can be done via an event driven approach, if so, prefer it unless there's a good reason not to, then explain why it was done in the summary

## Other conventions

- The app version shown in the About dialog comes from `Cargo.toml`; bump it as part of cutting a release.
- Configuration schema changes must remain backward compatible or handled by the corruption/defaults recovery path in `src/config.rs`; document new settings in the README.
