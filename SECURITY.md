# Security

## Tenant isolation
Two layers: every handler runs in a tenant-scoped transaction (`set_config('app.current_org_id', …, true)`), and PostgreSQL RLS (`FORCE ROW LEVEL SECURITY`, policies keyed on that setting) enforces the boundary for the least-privilege `xflow_app` role regardless of application bugs. Automated proof: `backend/tests/http_isolation.rs` and `workflow_flow.rs` (404 on every tenant resource, RLS WITH CHECK, sessions without context see nothing).
Foreign keys are checked with owner rights and bypass RLS, so every endpoint that accepts a tenant-owned id first verifies it with `ensure_owned` (foreign ids → 404). The background worker's only cross-tenant capability is two narrow SECURITY DEFINER functions touching `monitoring_jobs`.

## Authentication, sessions, RBAC
Argon2id; opaque 32-byte tokens, only the SHA-256 stored, revoked on logout, expiry checked every request; identical error for unknown email / wrong password; emails case-insensitive and unique. Roles OWNER/ADMIN/MEMBER/REVIEWER are enforced per endpoint via `rbac::Permission` (matrix in `rbac/mod.rs`, tested). Credit verification is OWNER/ADMIN only.

## Web
Browsers never see the API token: it lives in an httpOnly, SameSite=Lax (Secure in production) cookie and the Next.js proxy adds it server-side. State-changing proxy/auth calls must be same-origin: a mismatching `Origin` always fails, then `Sec-Fetch-Site` decides (browsers always send it and cannot forge it), then `Referer`; with no browser signals only GET/HEAD pass (`apps/web/lib/server.ts`, unit-tested). The proxy refuses the Paddle webhook path. The API sends no CORS headers unless `CORS_ALLOWED_ORIGINS` is set. Security headers (nosniff, frame deny) on the web app; downloads are `attachment` + `nosniff`.

## Money and input
No floats in financial paths. Inputs are decimal strings (floats rejected, ≤4 places, range-checked); output is canonical fixed 4-decimal strings. Request bodies are capped (2 MB; uploads 25 MB), uploads are validated by type and magic bytes, deduplicated by SHA-256, and stored under server-generated keys.

## Billing webhook
Unauthenticated by design; authenticity is HMAC-SHA256 over the raw body with a bounded timestamp window, constant-time compare, secret-rotation support, idempotent event ids, stale-event protection, and 503 when no secret is configured. Organization linking is signed too: `custom_data.organization_id` is only trusted with a backend-issued `organization_sig` (so nobody can attach a checkout to another organization and later cancel it to revoke that organization's access), a Paddle-signed subscription id outranks `custom_data`, and an organization with an active subscription ignores events from a different subscription id.

## Documents / prompt injection
PDFs are parsed by poppler `pdftotext` in a child process with a fixed argument list (no shell), a timeout and an output cap; run the backend as a non-root user in a container (the provided image does). Extraction output is stored as inert data for human review and never drives calculations; the engine only reads values a person entered.

## Known gaps (not fixed)
No rate limiting or brute-force lockout (also none on the PDF-parsing path); no password reset/email verification/MFA; no per-request CSRF token beyond Origin checking and SameSite; no dependency/vuln scanning in CI; the `xflow_app` role ships with a placeholder password until `XFLOW_APP_PASSWORD` is applied; local-filesystem storage only; Docker/CI unverified. A full threat-model checklist has not been independently reviewed.
