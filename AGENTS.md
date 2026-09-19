# AGENTS.md

## Project boundary

- CPA Portal is a GitHub-authenticated self-service layer around CPA. It provisions and stores encrypted CPA API keys, serves the user-facing portal, and exposes a small internal Admin API for trusted callers.
- CPA remains responsible for model requests, routing, credential management, and management operations. CPA Portal must not become a model-request proxy.
- CPA Usage Keeper remains responsible for usage aggregation, quota refresh, and quota history. Portal may proxy narrowly scoped read-only data through its authenticated internal client, but must not duplicate Keeper history locally.

## Architecture and data boundaries

- `src/routes/` owns HTTP routing and request-boundary authentication. Internal Admin API routes require the configured Bearer token and must retain `Cache-Control: no-store`.
- `src/services/` owns application composition and response shaping; `src/clients/cpa.rs` and `src/clients/keeper/` own upstream protocols. Keep upstream credentials inside these clients and never expose them in responses or logs.
- `/api/admin/v1/quota` reads Keeper's cached quota snapshot and must not trigger an official quota refresh.
- `/api/admin/v1/quota/history/{auth_index}` is an on-demand, single-auth-index Keeper history lookup. Keep Keeper's history JSON compatible and do not use Portal's response timestamp as an observation timestamp.
- `/api/admin/v1/quota/routing` exposes only an allowlisted, account-level CPA scheduling view. It is read-only and must not clear cooldowns or change routing state.
- `auth_index` identifies an upstream auth file. Validate it before placing it in a URL path; use URL path-segment APIs rather than string concatenation.
- `migrations/` and the SQLite database are a persistence boundary. Any schema change affects existing deployments and requires an explicit migration plan; do not add local quota-history storage when an upstream history source exists.

## Implementation rules

- Follow the existing Rust error boundary: convert upstream failures to `AppError`, and map route failures through `AdminApiError` with stable public error codes and generic public messages.
- Preserve opaque `serde_json::Value` pass-through at Keeper compatibility boundaries unless Portal must enforce a documented invariant. Avoid making a Keeper schema change turn unrelated Admin API routes into protocol failures.
- Keep secrets out of logs, errors, templates, serialized structs, and debug output. For new upstream management responses, define an explicit allowlist of fields rather than forwarding credential records wholesale.
- When adding or renaming configuration keys, update `config.example.toml` and the owning config type together. Keep the persistent encryption key stable; changing it makes stored API keys unrecoverable.
- Template changes must be checked against the Askama context and render types in `src/templates.rs`. Do not add route behavior only to a template or only to a handler.
- Use the repository's nightly rustfmt setup; `rustfmt.toml` enables unstable formatting options.
- Do not write unit tests unless the user explicitly requests them. Prefer the smallest existing check or a focused smoke scenario for behavior verification.

## README maintenance

- Keep `README.md` a concise Chinese introduction and deployment entry point, not an implementation manual or a changelog. Retain project purpose, essential setup, and operational constraints that readers need to use the service.
- Update it when user-facing behavior or deployment requirements make existing instructions inaccurate. A code change alone does not require a README addition; prefer correcting an existing sentence over adding a section.
- Refer to `config.example.toml` for the complete configuration instead of duplicating it. Keep deployment examples minimal and avoid repeating the same information across feature lists, route lists, and prose.
- Do not append response-field inventories, internal algorithms, cache/history semantics, edge-case explanations, or session-specific findings for each new feature. Keep those details with the relevant code or an existing dedicated document when needed; do not create another document merely to relocate removed README text.
- Preserve essential setup and data-loss warnings, including OAuth callback configuration, database persistence, encryption-key preservation, and the internal-only Admin API boundary. Remove redundant explanations without losing these requirements.

## Verification

- For Rust source changes, use the checks defined in `.github/workflows/ci.yml`: nightly `cargo fmt --all -- --check` and locked Clippy with warnings denied.
- For route, upstream-client, serialization, or configuration changes, verify the affected boundary specifically; include authentication, no-store behavior, error mapping, and sensitive-field filtering when relevant.
- For persistence changes, verify migration behavior against an existing SQLite database rather than only a fresh database.
- Do not modify deployment state, publish images, or change remote data unless the user explicitly asks for that operation.
