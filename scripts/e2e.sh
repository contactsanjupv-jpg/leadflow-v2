#!/bin/sh
# Browser end-to-end test of the critical customer workflow against REAL servers:
# Rust API (real Postgres, RLS, billing enforcement ON) + the production Next.js build + headless Chromium.
# Prerequisites: a migrated database reachable as the xflow_app role (DATABASE_URL), a built backend binary
# (BACKEND_BIN, default release) and a built web app (npm run build in apps/web).
# Paddle.js itself is stubbed in the browser (cdn.paddle.com is not reachable from CI/sandboxes and a real
# checkout needs a card); the signed webhook that activates the subscription is sent to the real endpoint.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
: "${DATABASE_URL:?DATABASE_URL (xflow_app role) must be set}"
BACKEND_BIN="${BACKEND_BIN:-$ROOT/backend/target/release/xflow-backend}"
API_PORT="${E2E_API_PORT:-18080}"; WEB_PORT="${E2E_WEB_PORT:-13000}"
SECRET="e2e-webhook-secret"
STORE="$(mktemp -d)"
PIDS=""
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null || true; done; rm -rf "$STORE"; }
trap cleanup EXIT INT TERM

BIND_ADDR="127.0.0.1:$API_PORT" PADDLE_WEBHOOK_SECRET="$SECRET" BILLING_ENFORCEMENT=true WORKER_ENABLED=false STORAGE_DIR="$STORE" \
  "$BACKEND_BIN" >"$STORE/api.log" 2>&1 &
PIDS="$PIDS $!"
( cd "$ROOT/apps/web" && API_INTERNAL_URL="http://127.0.0.1:$API_PORT" INSECURE_COOKIES=1 PADDLE_CLIENT_TOKEN=test_e2e_token PADDLE_PRICE_ID=pri_e2e_monthly PADDLE_ENV=sandbox \
  exec ./node_modules/.bin/next start -p "$WEB_PORT" >"$STORE/web.log" 2>&1 ) &
PIDS="$PIDS $!"

i=0
until curl -fs "http://127.0.0.1:$API_PORT/api/v1/health" >/dev/null && curl -fs "http://127.0.0.1:$WEB_PORT/login" >/dev/null; do
  i=$((i+1)); [ "$i" -gt 60 ] && { echo "servers did not start"; tail -20 "$STORE/api.log" "$STORE/web.log"; exit 1; }
  sleep 1
done
cd "$ROOT/apps/web"
E2E_WEB_URL="http://127.0.0.1:$WEB_PORT" E2E_API_URL="http://127.0.0.1:$API_PORT" E2E_WEBHOOK_SECRET="$SECRET" npm run test:e2e
