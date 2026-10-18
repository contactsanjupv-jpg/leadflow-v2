import test from "node:test";
import assert from "node:assert/strict";
import { NextRequest } from "next/server";
import { gateRedirect, LOGIN_PATH, SESSION_EXPIRED_PATH } from "./session-redirect.ts";
import { middleware } from "../middleware.ts";

const q = (s = "") => new URLSearchParams(s);

test("no cookie: protected pages go to /login, the login page is reachable", () => {
  assert.equal(gateRedirect(false, "/", q()), "/login");
  assert.equal(gateRedirect(false, "/invoices", q()), "/login");
  assert.equal(gateRedirect(false, "/login", q()), null);
  assert.equal(gateRedirect(false, "/login", q("expired=1")), null);
});

test("cookie present: app pages pass through and a normal visit to /login skips the form", () => {
  assert.equal(gateRedirect(true, "/", q()), null);
  assert.equal(gateRedirect(true, "/invoices", q()), null);
  assert.equal(gateRedirect(true, "/login", q()), "/", "an already logged-in user is still sent to the app");
});

test("the expired-session landing page is NOT bounced away while the stale cookie is present", () => {
  assert.equal(gateRedirect(true, "/login", q("expired=1")), null);
});

test("no redirect loop: following the layout's expired-session redirect with a stale cookie terminates at once", () => {
  // Browser model: cookie is present throughout. Layout (API said 401) -> SESSION_EXPIRED_PATH, then the gate decides.
  const url = new URL(SESSION_EXPIRED_PATH, "http://app.test");
  assert.equal(url.pathname, LOGIN_PATH);
  assert.equal(gateRedirect(true, url.pathname, url.searchParams), null, "gate must not redirect the layout's target");
  // And entering via any path settles within a couple of hops: "/" (gate ok) -> layout 401 -> expired page (gate ok).
  const hops: string[] = [];
  let at = "/login"; // user opens the bare login page with a stale cookie
  for (let i = 0; i < 10; i++) {
    const u = new URL(at, "http://app.test");
    const next = gateRedirect(true, u.pathname, u.searchParams);
    if (next === null) {
      // page would render; for "/" the layout rejects the stale session and redirects to the expired landing page
      if (u.pathname === "/") { hops.push("layout->" + SESSION_EXPIRED_PATH); at = SESSION_EXPIRED_PATH; continue; }
      break;
    }
    hops.push("gate->" + next); at = next;
  }
  assert.equal(at, SESSION_EXPIRED_PATH);
  assert.ok(hops.length <= 3, `settled in ${hops.length} hops: ${hops.join(", ")}`);
});

test("middleware.ts applies the gate (wiring, not just the pure function)", () => {
  const req = (path: string, cookie?: string) =>
    new NextRequest(`http://app.test${path}`, cookie ? { headers: { cookie } } : undefined);
  const loc = (r: Response) => r.headers.get("location");

  const noCookieApp = middleware(req("/invoices"));
  assert.equal(noCookieApp.status, 307);
  assert.equal(loc(noCookieApp), "http://app.test/login");

  const cookieLogin = middleware(req("/login", "xflow_session=abc"));
  assert.equal(cookieLogin.status, 307);
  assert.equal(loc(cookieLogin), "http://app.test/");

  const staleLanding = middleware(req("/login?expired=1", "xflow_session=stale"));
  assert.equal(staleLanding.headers.get("x-middleware-next"), "1", "stale-session landing page must pass through");
  assert.equal(loc(staleLanding), null);

  const emptyCookie = middleware(req("/", "xflow_session="));
  assert.equal(loc(emptyCookie), "http://app.test/login", "an empty cookie counts as no session");
});
