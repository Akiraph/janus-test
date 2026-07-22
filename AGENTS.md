# Janus Repository Instructions

## Public repository boundary

- This repository must be understandable on its own. Do not link to files, specifications, or directories that are not committed here.
- Do not expose internal planning labels, milestone names, task IDs, or temporary implementation codenames in source code, public documentation, generated contracts, API payloads, or UI copy.
- Prefer durable capability names such as `platform`, `events`, `bootstrap`, and `workspace` over delivery-phase labels.
- A `module.toml` field may remain empty when its authoritative source is not present in this repository. Do not invent references to satisfy a checker.

## Architecture

- The backend is one Rust server crate containing seven domain Modules: identity, models, projects, sessions, supervisor, runtime, and workspace-sync.
- Each Module exposes only `interface.rs`, includes a short `README.md` and `module.toml`, and does not import another Module's private implementation.
- Cross-module workflows belong in `application/`; shared persistence, IDs, clock, events, and secrets belong in `platform/`; external implementations belong in `adapters/`; HTTP/SSE belongs in `transport/`.
- Do not introduce a central ports crate, generic repository abstraction, global event bus, or service locator.

## Quality

- Rust must pass formatting, Clippy with warnings denied, and workspace tests.
- Frontend code must pass TypeScript, Biome, build, accessibility, responsive, and reduced-motion checks.
- Public HTTP types originate in Rust and generate OpenAPI and TypeScript. The test CLI uses only public network interfaces.
- Use one Bun lockfile. Do not add npm, pnpm, or Yarn lockfiles.
