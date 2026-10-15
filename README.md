# XFlow V2 — Hazardous-Waste Vendor Bill Reconciliation

Continuously checks contract rates, manifests and weight tickets against vendor invoices, surfaces billing discrepancies for review, produces evidence packages, and tracks disputes through verified credit recovery. See **BUILD_NOTES.md** for the exact, current implementation state and known gaps.

Stack: Rust (axum, sqlx) modular monolith · PostgreSQL with row-level security · Next.js/TypeScript · Paddle (webhooks).

## Local setup
```
apt-get install -y rustc cargo postgresql postgresql-client libssl-dev pkg-config build-essential poppler-utils
service postgresql start
su - postgres -c "createdb xflow_dev"
MIGRATION_DATABASE_URL=postgres://postgres:...@127.0.0.1/xflow_dev sh scripts/migrate.sh
export DATABASE_URL=postgres://xflow_app:CHANGE_ME_IN_PRODUCTION@127.0.0.1/xflow_dev
cd backend && cargo test --locked          # needs the database above
BILLING_ENFORCEMENT=false cargo run        # API on :8080
cd ../apps/web && npm ci && npm run dev    # UI on :3000, proxies to the API
```
Browser end-to-end test (needs the built backend + `npm run build`): `DATABASE_URL=... BACKEND_BIN=backend/target/debug/xflow-backend sh scripts/e2e.sh`.

`scripts/seed_demo.sql` loads the spec §55 example ($700 discrepancy) for an existing organization.

## Layout
`backend/src`: `common` (money) · `reconciliation` (engine + service) · `evidence_pdf` · `http` (handlers) · `rbac` · `audit` · `billing` · `discrepancies`/`disputes` (state machines) · `storage`/`extraction` (provider traits) · `workers` · `tenants`/`auth`/`config`. `migrations/` · `apps/web` · `docker/` · `.github/workflows/`.
