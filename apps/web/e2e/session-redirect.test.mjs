// Regression: a stale / expired / revoked `xflow_session` cookie must land on the login page, never in an
// endless "/" <-> "/login" redirect loop, while valid sessions and normal login keep working.
//
// Cause being guarded: the cookie is still PRESENT, so the middleware used to bounce /login -> "/", while
// the app layout (API said 401) bounced "/" -> /login. Now the layout targets /login?expired=1, which the
// middleware lets through. Server-side auth is unchanged: protected pages never render without a valid session.
import test from "node:test";
import assert from "node:assert/strict";
import { chromium as pw } from "playwright-core";
import chromiumBinary from "@sparticuz/chromium";

const WEB = process.env.E2E_WEB_URL ?? "http://127.0.0.1:13000";
const API = process.env.E2E_API_URL ?? "http://127.0.0.1:18080";
const PASSWORD = "a-sufficiently-long-password";

async function launch() {
  return process.platform === "linux"
    ? pw.launch({ args: chromiumBinary.args, executablePath: await chromiumBinary.executablePath(), headless: true })
    : pw.launch({ args: ["--no-sandbox"], headless: true });
}

/** Navigate and return the redirect responses seen. A real loop makes goto() throw ERR_TOO_MANY_REDIRECTS. */
async function visit(page, path) {
  const redirects = [];
  const onResponse = (r) => { if (r.status() >= 300 && r.status() < 400) redirects.push(`${r.status()} ${new URL(r.url()).pathname}${new URL(r.url()).search}`); };
  page.on("response", onResponse);
  try {
    await page.goto(`${WEB}${path}`, { waitUntil: "load", timeout: 30000 });
  } catch (e) {
    assert.fail(`navigating to ${path} did not settle (redirect loop?): ${e.message}; redirects so far: ${redirects.join(" | ")}`);
  } finally {
    page.off("response", onResponse);
  }
  return redirects;
}

async function signUp(page, email) {
  await page.goto(`${WEB}/login`);
  await page.getByRole("button", { name: "Create an account" }).click();
  await page.getByLabel("Organization").fill("Session Redirect Co");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel(/Password/).fill(PASSWORD);
  await page.getByRole("button", { name: "Create account" }).click();
  await page.waitForURL(`${WEB}/`);
  await page.getByRole("heading", { name: "Dashboard" }).waitFor();
}

const sessionCookie = async (ctx) => (await ctx.cookies(WEB)).find((c) => c.name === "xflow_session");

async function assertOnExpiredLogin(page, redirects) {
  const u = new URL(page.url());
  assert.equal(u.pathname, "/login", `ended on ${page.url()} after ${redirects.join(" | ")}`);
  assert.equal(u.searchParams.get("expired"), "1");
  assert.ok(redirects.length <= 4, `too many hops (${redirects.length}): ${redirects.join(" | ")}`);
  await page.getByLabel("Email").waitFor();
  assert.equal(await page.getByRole("button", { name: "Log out" }).count(), 0, "no protected UI may render with a rejected session");
}

// Each test owns its browser so a failure (e.g. a redirect loop) cannot contaminate the others.
async function withPage(fn) {
  const browser = await launch();
  try {
    const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
    const page = await ctx.newPage();
    page.setDefaultTimeout(20000);
    await fn({ browser, ctx, page });
  } finally {
    await browser.close();
  }
}

test("garbage/stale cookie: no loop on /, a protected page, or /login; lands on the login form", () =>
  withPage(async ({ ctx, page }) => {
    await ctx.addCookies([{ name: "xflow_session", value: "stale-or-forged-session-token", url: WEB, httpOnly: true, sameSite: "Lax" }]);
    for (const path of ["/", "/invoices", "/login"]) {
      const redirects = await visit(page, path);
      await assertOnExpiredLogin(page, redirects);
    }
  }));

test("a REAL session revoked server-side (expired) lands on login without looping, then login works again", () =>
  withPage(async ({ ctx, page }) => {
    const email = `loop-revoked-${Date.now()}@example.test`;
    await signUp(page, email);
    const cookie = await sessionCookie(ctx);
    assert.ok(cookie, "signup must set the session cookie");
    const out = await fetch(`${API}/api/v1/auth/logout`, { method: "POST", headers: { authorization: `Bearer ${cookie.value}` } });
    assert.ok(out.ok, `API logout failed: ${out.status}`);
    assert.ok(await sessionCookie(ctx), "browser still holds the now-invalid cookie");
    const redirects = await visit(page, "/");
    await assertOnExpiredLogin(page, redirects);
    // recovery: logging in from the expired landing page replaces the stale cookie
    await page.getByLabel("Email").fill(email);
    await page.getByLabel(/Password/).fill(PASSWORD);
    await page.getByRole("button", { name: "Log in" }).click();
    await page.waitForURL(`${WEB}/`);
    await page.getByRole("heading", { name: "Dashboard" }).waitFor();
    assert.notEqual((await sessionCookie(ctx)).value, cookie.value, "login must replace the stale cookie");
  }));

test("valid session and normal login still work; anonymous visitors are still blocked", () =>
  withPage(async ({ ctx, page }) => {
    const email = `loop-valid-${Date.now()}@example.test`;
    await signUp(page, email);
    await visit(page, "/invoices");
    assert.equal(new URL(page.url()).pathname, "/invoices");
    await page.getByRole("button", { name: "Log out" }).waitFor();
    // a logged-in user opening /login is still sent straight to the app
    const r = await visit(page, "/login");
    assert.equal(new URL(page.url()).pathname, "/", `expected redirect to app, got ${page.url()} via ${r.join(" | ")}`);
    await page.getByRole("button", { name: "Log out" }).waitFor();
    // log out -> login page, cookie gone
    await page.getByRole("button", { name: "Log out" }).click();
    await page.waitForURL(`${WEB}/login`);
    assert.equal(await sessionCookie(ctx), undefined);
    // normal login with the same credentials
    await page.getByLabel("Email").fill(email);
    await page.getByLabel(/Password/).fill(PASSWORD);
    await page.getByRole("button", { name: "Log in" }).click();
    await page.waitForURL(`${WEB}/`);
    await page.getByRole("heading", { name: "Dashboard" }).waitFor();
    // an anonymous visitor (cookie cleared) is still blocked from protected pages with the plain login page.
    // (Same context on purpose: the bundled Chromium runs --single-process, so closing a 2nd context kills the browser.)
    await ctx.clearCookies();
    await visit(page, "/invoices");
    assert.equal(new URL(page.url()).pathname, "/login");
    assert.equal(new URL(page.url()).searchParams.get("expired"), null, "anonymous bounce is the plain login page");
    assert.equal(await page.getByRole("button", { name: "Log out" }).count(), 0);
  }));
