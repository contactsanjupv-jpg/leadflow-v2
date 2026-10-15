// Critical customer workflow in a real browser:
// sign up -> blocked without subscription -> Paddle checkout (stubbed Paddle.js, REAL signed webhook) ->
// enter vendor/contract/manifest/ticket/invoice -> reconcile -> review -> evidence PDF -> dispute through
// verified credit -> document upload with manual fallback -> log out.
import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import { chromium as pw } from "playwright-core";
import chromiumBinary from "@sparticuz/chromium";

const WEB = process.env.E2E_WEB_URL ?? "http://127.0.0.1:13000";
const API = process.env.E2E_API_URL ?? "http://127.0.0.1:18080";
const SECRET = process.env.E2E_WEBHOOK_SECRET ?? "e2e-webhook-secret";
const ART = process.env.E2E_ARTIFACTS ?? "/tmp/e2e-artifacts";
fs.mkdirSync(ART, { recursive: true });

const PADDLE_STUB = `
window.__paddle = { env: null, init: null, opened: null };
window.Paddle = {
  Environment: { set(e) { window.__paddle.env = e; } },
  Initialize(o) { window.__paddle.init = o; },
  Checkout: { open(o) { window.__paddle.opened = o; } },
};`;

function signedHeader(body) {
  const ts = Math.floor(Date.now() / 1000);
  const h1 = crypto.createHmac("sha256", SECRET).update(`${ts}:${body}`).digest("hex");
  return `ts=${ts};h1=${h1}`;
}

async function sendPaddleWebhook(eventType, status, customData, subId, occurredAt) {
  const body = JSON.stringify({
    event_id: `evt_${crypto.randomUUID()}`, event_type: eventType, occurred_at: occurredAt, notification_id: `ntf_${crypto.randomUUID()}`,
    data: { id: subId, status, customer_id: "ctm_e2e", items: [{ status, quantity: 1, price: { id: "pri_e2e_monthly" } }],
            current_billing_period: { starts_at: occurredAt, ends_at: "2030-01-01T00:00:00Z" }, custom_data: customData },
  });
  const res = await fetch(`${API}/api/v1/billing/paddle/webhook`, { method: "POST", headers: { "content-type": "application/json", "paddle-signature": signedHeader(body) }, body });
  return { status: res.status, json: await res.json() };
}

test("critical customer workflow", async (t) => {
  const browser =
  process.platform === "linux"
    ? await pw.launch({
        args: chromiumBinary.args,
        executablePath: await chromiumBinary.executablePath(),
        headless: true,
      })
    : await pw.launch({
        args: ["--no-sandbox"],
        headless: true,
      });
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 1000 }, acceptDownloads: true });
  const page = await ctx.newPage();
  page.setDefaultTimeout(20000);
  const problems = [];
  page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
  page.on("console", (m) => { if (m.type() === "error") problems.push(`console: ${m.text()}`); });
  await page.route("https://cdn.paddle.com/**", (route) => route.fulfill({ contentType: "application/javascript", body: PADDLE_STUB }));

  const stepNames = [];
  const step = async (name, fn) => { stepNames.push(name); try { await fn(); } catch (e) { await page.screenshot({ path: `${ART}/FAIL-${stepNames.length}-${name.replace(/\W+/g, "_")}.png`, fullPage: true }); throw new Error(`step "${name}" failed: ${e.message}`); } };
  const card = (title) => page.locator("section.card", { has: page.locator("h2", { hasText: title }) });
  const fillLabel = (scope, label, value) => scope.getByLabel(label, { exact: true }).fill(value);

  try {
    const stamp = Date.now();
    const email = `e2e-${stamp}@example.test`;

    await step("anonymous visitors are sent to login", async () => {
      await page.goto(`${WEB}/`);
      await page.waitForURL("**/login");
    });

    await step("sign up", async () => {
      await page.getByRole("button", { name: "Create an account" }).click();
      await page.getByLabel("Organization").fill("E2E Waste Co");
      await page.getByLabel("Email").fill(email);
      await page.getByLabel(/Password/).fill("a-sufficiently-long-password");
      await page.getByRole("button", { name: "Create account" }).click();
      await page.waitForURL(`${WEB}/`);
      await page.getByRole("heading", { name: "Dashboard" }).waitFor();
    });

    await step("without a subscription the banner shows and processing is refused server-side (402)", async () => {
      await page.getByTestId("no-subscription-banner").waitFor();
      const status = await page.evaluate(() => fetch("/api/proxy/reconciliations/run", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ invoice_id: "00000000-0000-0000-0000-000000000000" }) }).then((r) => r.status));
      assert.equal(status, 402);
    });

    let customData;
    await step("Paddle checkout opens with the organization id + signature in custom_data", async () => {
      await page.goto(`${WEB}/settings`);
      await page.getByRole("button", { name: "Subscribe" }).click();
      await page.waitForFunction(() => window.__paddle?.opened);
      const paddle = await page.evaluate(() => window.__paddle);
      assert.equal(paddle.env, "sandbox");
      assert.equal(paddle.init.token, "test_e2e_token");
      assert.deepEqual(paddle.opened.items, [{ priceId: "pri_e2e_monthly", quantity: 1 }]);
      assert.equal(paddle.opened.customer.email, email);
      customData = paddle.opened.customData;
      const me = await page.evaluate(() => fetch("/api/proxy/me").then((r) => r.json()));
      assert.equal(customData.organization_id, me.organization_id);
      assert.match(customData.organization_sig, /^[0-9a-f]{64}$/);
    });

    await step("forged webhooks are rejected, then the REAL signed activation event unlocks the account", async () => {
      const bad = await fetch(`${API}/api/v1/billing/paddle/webhook`, { method: "POST", headers: { "content-type": "application/json", "paddle-signature": "ts=1;h1=00" }, body: "{}" });
      assert.equal(bad.status, 401);
      const forged = await sendPaddleWebhook("subscription.activated", "active", { organization_id: customData.organization_id, organization_sig: "0".repeat(64) }, `sub_forged_${stamp}`, new Date().toISOString());
      assert.equal(forged.json.status, "unmapped");
      assert.match(await page.getByTestId("billing-status").innerText(), /NONE/);

      const ok = await sendPaddleWebhook("subscription.activated", "active", customData, `sub_e2e_${stamp}`, new Date().toISOString());
      assert.deepEqual([ok.status, ok.json.status], [200, "applied"]);
      // Paddle.js would now emit checkout.completed; the page waits for the server-side status to flip.
      await page.evaluate(() => window.__paddle.init.eventCallback({ name: "checkout.completed" }));
      await page.getByText("Your subscription is active.").waitFor({ timeout: 30000 });
      await page.goto(`${WEB}/`);
      assert.equal(await page.getByTestId("no-subscription-banner").count(), 0);
    });

    await step("enter vendor, contract with rate + authorized fee", async () => {
      await page.goto(`${WEB}/data`);
      await page.getByRole("button", { name: "Vendors", exact: true }).click();
      await page.getByLabel("Name").fill("Acme Disposal");
      await page.getByRole("button", { name: "Add vendor" }).click();
      await page.getByText("Acme Disposal").waitFor();

      await page.getByRole("button", { name: "Contracts & rates" }).click();
      await page.getByLabel("Vendor").selectOption({ label: "Acme Disposal" });
      await page.getByLabel("Contract name").fill("Master Service Agreement");
      await page.getByRole("button", { name: "Add contract" }).click();
      const rates = card("Rates");
      await rates.waitFor();
      await fillLabel(rates, "Waste stream", "F003");
      await fillLabel(rates, "Unit", "drum");
      await fillLabel(rates, "Unit rate", "400.00");
      await fillLabel(rates, "Effective from", "2026-01-01");
      await rates.getByRole("button", { name: "Add authorized fee" }).click();
      await fillLabel(rates, "Fee code", "TRANSPORT");
      await fillLabel(rates, "Amount", "500.00");
      await rates.getByRole("button", { name: "Save rate" }).click();
      await rates.getByText("$400.00").waitFor();
      await rates.getByText("TRANSPORT").waitFor();
    });

    await step("enter manifest and weight ticket", async () => {
      await page.getByRole("button", { name: "Manifests", exact: true }).click();
      const m = card("Manifests");
      await fillLabel(m, "Manifest #", "M-E2E-1");
      await fillLabel(m, "Waste stream", "F003");
      await fillLabel(m, "Containers", "10");
      await fillLabel(m, "Quantity", "10");
      await fillLabel(m, "Unit", "drum");
      await fillLabel(m, "Shipment date", "2026-03-09");
      await m.getByRole("button", { name: "Add manifest" }).click();
      await m.getByRole("cell", { name: "M-E2E-1" }).waitFor();

      await page.getByRole("button", { name: "Weight tickets" }).click();
      const w = card("Weight tickets");
      await fillLabel(w, "Ticket #", "T-E2E-1");
      await w.getByLabel("Manifest").selectOption({ label: "M-E2E-1" });
      await fillLabel(w, "Net weight", "10");
      await fillLabel(w, "Unit", "drum");
      await fillLabel(w, "Containers", "10");
      await fillLabel(w, "Ticket date", "2026-03-10");
      await w.getByRole("button", { name: "Add ticket" }).click();
      await w.getByRole("cell", { name: "T-E2E-1" }).waitFor();
    });

    await step("enter the invoice (spec worked example: $450/drum, transport, unauthorized env fee)", async () => {
      await page.goto(`${WEB}/invoices`);
      const f = card("Enter an invoice");
      await f.getByLabel("Vendor").selectOption({ label: "Acme Disposal" });
      await fillLabel(f, "Invoice #", "INV-E2E-1");
      await fillLabel(f, "Invoice date", "2026-03-15");
      await fillLabel(f, "Service date", "2026-03-10");
      await fillLabel(f, "Waste stream", "F003");
      await f.getByLabel("Manifest").selectOption({ label: "M-E2E-1" });
      await fillLabel(f, "Invoice total", "5200.00");
      const lines = [["Drum disposal", "", "10", "drum", "450.00", "4500.00"], ["Transportation", "TRANSPORT", "1", "flat", "500.00", "500.00"], ["Environmental fee", "ENV_FEE", "1", "flat", "200.00", "200.00"]];
      for (let i = 1; i < lines.length; i++) await f.getByRole("button", { name: "Add line" }).click();
      for (let i = 0; i < lines.length; i++) {
        const inputs = f.locator("tbody tr").nth(i).locator("input");
        for (let j = 0; j < 6; j++) await inputs.nth(j).fill(lines[i][j]);
      }
      await f.getByRole("button", { name: "Save invoice" }).click();
      await f.getByText("Invoice saved").waitFor();
    });

    await step("run the reconciliation: expected $4,500, invoiced $5,200, discrepancy $700", async () => {
      await page.getByRole("link", { name: "INV-E2E-1" }).click();
      await page.getByRole("button", { name: "Run reconciliation" }).click();
      const rec = card("Latest reconciliation");
      await rec.waitFor();
      await rec.getByText("$4,500.00").first().waitFor();
      await rec.getByText("$5,200.00").first().waitFor();
      assert.match(await rec.innerText(), /Discrepancy\s*\$700\.00/);
      await rec.getByText("RULE_03_RATE_MISMATCH").first().waitFor();
      await rec.getByText("RULE_04_UNAUTHORIZED_FEE").first().waitFor();
    });

    await step("review queue: generate evidence, confirm the rate mismatch", async () => {
      await page.goto(`${WEB}/review`);
      const row = page.locator("tbody tr", { hasText: "contracted rate" }).first();
      await row.click();
      await page.getByRole("button", { name: "Generate evidence package" }).click();
      await page.getByText("Evidence package generated").waitFor();
      await page.getByRole("button", { name: "Confirm", exact: true }).click();
      await page.getByText("Saved").waitFor();
      await page.getByLabel("Status").selectOption("CONFIRMED");
      await page.locator("tbody tr", { hasText: "contracted rate" }).first().click();
    });

    await step("download the evidence PDF and check it is a real PDF with the case data", async () => {
      const [download] = await Promise.all([page.waitForEvent("download"), page.getByTestId("evidence-pdf-link").click()]);
      const path = `${ART}/evidence.pdf`;
      await download.saveAs(path);
      const bytes = fs.readFileSync(path);
      assert.equal(bytes.subarray(0, 8).toString("latin1"), "%PDF-1.4");
      const text = bytes.toString("latin1");
      for (const needle of ["INV-E2E-1", "RULE_03_RATE_MISMATCH", "$500.00", "$5,200.00", "$4,500.00", "M-E2E-1", "T-E2E-1"]) assert.ok(text.includes(needle), `PDF is missing ${needle}`);
      assert.match(download.suggestedFilename(), /^evidence-INV-E2E-1-.*\.pdf$/);
    });

    await step("open a dispute and take it through verified credit recovery", async () => {
      await page.getByRole("button", { name: "Open dispute" }).click();
      await page.goto(`${WEB}/disputes`);
      await page.locator("tbody tr", { hasText: "Acme Disposal" }).first().click();
      for (const to of ["PREPARE", "REVIEW", "SUBMITTED", "VENDOR RESPONDED"]) await page.getByRole("button", { name: `→ ${to}` }).click().then(() => page.waitForTimeout(150));
      await page.getByLabel("Credit promised by vendor").fill("500.00");
      await page.getByRole("button", { name: "→ CREDIT PROMISED" }).click();
      await page.getByLabel("Credit actually received").fill("500.00");
      await page.getByLabel("Credit memo reference").fill("CM-E2E-9001");
      await page.getByRole("button", { name: "→ CREDIT RECEIVED" }).click();
      await page.getByText("CM-E2E-9001").waitFor();
      await page.getByRole("button", { name: "→ VERIFIED" }).click();
      await page.getByRole("button", { name: "Close", exact: true }).click();
      const summary = card("Recovery summary");
      await summary.getByText("Closed.").or(page.getByText("Closed.")).first().waitFor();
      await page.reload();
      const s = card("Recovery summary");
      const recovered = s.locator(".stat", { hasText: "Recovered (verified)" });
      await recovered.getByText("$500.00").waitFor();
    });

    await step("documents: text file is extracted, a scan/photo falls back to manual entry and can be resolved", async () => {
      await page.goto(`${WEB}/documents`);
      await page.getByTestId("document-file").setInputFiles({ name: `notes-${stamp}.txt`, mimeType: "text/plain", buffer: Buffer.from(`Manifest notes ${stamp}\n10 drums F003`) });
      await page.getByRole("button", { name: "Upload" }).click();
      await page.getByText("Extracted text (plain_text)").waitFor();
      assert.match(await page.getByTestId("extracted-text").innerText(), /10 drums F003/);

      const png = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.from(`photo-${stamp}`)]);
      await page.getByTestId("document-file").setInputFiles({ name: `scan-${stamp}.png`, mimeType: "image/png", buffer: png });
      await page.getByRole("button", { name: "Upload" }).click();
      await page.getByText(/could not be read automatically/).waitFor();
      const row = page.locator("tbody tr", { hasText: `scan-${stamp}.png` });
      assert.match(await row.innerText(), /REVIEW REQUIRED/);
      assert.match(await row.innerText(), /manually/);
      await page.getByRole("button", { name: "Mark as manually processed" }).click();
      await page.locator("tbody tr", { hasText: `scan-${stamp}.png` }).locator(".badge", { hasText: "PROCESSED" }).waitFor();
    });

    await step("log out ends the session", async () => {
      await page.getByRole("button", { name: "Log out" }).click();
      await page.waitForURL("**/login");
      const status = await page.evaluate(() => fetch("/api/proxy/dashboard").then((r) => r.status));
      assert.equal(status, 401);
    });

    // Allow-listed, known-benign noise: expected 401/402/404 responses that the test provokes on purpose, and
    // Next.js falling back to a full page load when a <Link> prefetch is aborted by a navigation.
    const real = problems.filter((p) => !/favicon|Failed to load resource: the server responded with a status of (401|402|404)|Failed to fetch RSC payload .* Falling back to browser navigation/.test(p));
    assert.deepEqual(real, [], `browser reported errors:\n${real.join("\n")}`);
    console.log(`E2E OK: ${stepNames.length} steps passed`);
  } finally {
    await browser.close();
  }
});
