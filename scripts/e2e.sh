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

# Never test somebody else's servers: the readiness check below accepts ANY process answering on these
# ports, so a leftover server from an earlier run (possibly an older build or another database) would be
# tested instead of the one started here, while this script's own servers fail to bind and die silently.
# curl exit 7 is "connection refused": the port is free. Anything else means something is listening.
port_busy() { rc=0; curl -s -m 3 -o /dev/null "http://127.0.0.1:$1/" >/dev/null 2>&1 || rc=$?; [ "$rc" -ne 7 ]; }
if port_busy "$API_PORT" || port_busy "$WEB_PORT"; then
  echo "port $API_PORT (API) or $WEB_PORT (web) is already in use: a server from an earlier run is probably still running." >&2
  echo "Stop it (for example: lsof -nP -iTCP:$API_PORT -iTCP:$WEB_PORT -sTCP:LISTEN) or set E2E_API_PORT / E2E_WEB_PORT, then retry." >&2
  exit 1
fi
[ -x "$BACKEND_BIN" ] || { echo "BACKEND_BIN is not an executable file: $BACKEND_BIN (build it, or set BACKEND_BIN)" >&2; exit 1; }
show_logs() { echo "--- API log (last 40 lines)"; tail -40 "$STORE/api.log" 2>/dev/null || true; echo "--- web log (last 40 lines)"; tail -40 "$STORE/web.log" 2>/dev/null || true; }

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

# Preflight: do exactly what every browser test does first (sign up through the web app, land on "/") but
# with curl, so a failure shows the HTTP status, response body, cookie attributes and server logs instead
# of a 20 second locator timeout. The app layout renders no page content (so no "Dashboard" heading) when
# GET /api/v1/me answers anything but 200 or 401, which is why /me is checked explicitly.
WEB="http://127.0.0.1:$WEB_PORT"
PF_EMAIL="e2e-preflight-$$-$(date +%s)@example.test"
pf_fail() { echo "PREFLIGHT FAILED: $1" >&2; show_logs >&2; exit 1; }
pf_status=$(curl -sS -m 30 -D "$STORE/pf.headers" -o "$STORE/pf.signup" -w '%{http_code}' -c "$STORE/pf.jar" \
  -H 'content-type: application/json' -H "origin: $WEB" \
  -d "{\"organization_name\":\"E2E Preflight\",\"email\":\"$PF_EMAIL\",\"password\":\"e2e-preflight-password-1\"}" \
  "$WEB/api/auth/signup") || pf_fail "signup request did not complete"
[ "$pf_status" = "200" ] || { echo "signup -> HTTP $pf_status: $(cat "$STORE/pf.signup")" >&2; pf_fail "signup was not accepted"; }
grep -qi '^set-cookie: xflow_session=' "$STORE/pf.headers" || { echo "signup headers:" >&2; cat "$STORE/pf.headers" >&2; pf_fail "signup did not set the session cookie"; }
grep -i '^set-cookie: xflow_session=' "$STORE/pf.headers" | grep -qi 'secure' && pf_fail "the session cookie is marked Secure on plain http, so a browser would drop it (is INSECURE_COOKIES=1 reaching the web server?)"
pf_me=$(curl -sS -m 30 -b "$STORE/pf.jar" -o "$STORE/pf.me" -w '%{http_code}' "$WEB/api/proxy/me") || pf_fail "GET /api/v1/me did not complete"
[ "$pf_me" = "200" ] || { echo "GET /api/v1/me (right after a successful signup) -> HTTP $pf_me: $(cat "$STORE/pf.me")" >&2; pf_fail "the API cannot serve the new user's profile, so the app layout shows 'The API is unavailable' and never renders the Dashboard"; }
curl -sS -m 30 -b "$STORE/pf.jar" -o "$STORE/pf.home" "$WEB/" || pf_fail "GET / did not complete"
grep -q '<h1>Dashboard</h1>' "$STORE/pf.home" || { grep -o 'The API is unavailable[^<]*' "$STORE/pf.home" >&2 || true; pf_fail "'/' after signup does not contain the Dashboard heading"; }
echo "preflight OK: signup -> 200 + session cookie, /api/v1/me -> 200, Dashboard rendered"

cd "$ROOT/apps/web"
E2E_WEB_URL="$WEB" E2E_API_URL="http://127.0.0.1:$API_PORT" E2E_WEBHOOK_SECRET="$SECRET" npm run test:e2e || { rc=$?; echo "e2e FAILED (exit $rc)" >&2; show_logs >&2; exit "$rc"; }
