import test from "node:test";
import assert from "node:assert/strict";
import { sameOrigin } from "./server.ts";

const mk = (method: string, headers: Record<string, string>) => new Request("http://app.test/api/x", { method, headers: { host: "app.test", ...headers } });

test("same-origin browser POSTs pass whether or not the browser sends Origin", () => {
  assert.ok(sameOrigin(mk("POST", { origin: "http://app.test", "sec-fetch-site": "same-origin" })));
  assert.ok(sameOrigin(mk("POST", { "sec-fetch-site": "same-origin" })), "Chromium builds that omit Origin on same-origin POSTs");
  assert.ok(sameOrigin(mk("POST", { origin: "http://app.test" })));
  assert.ok(sameOrigin(mk("POST", { referer: "http://app.test/login" })));
});

test("cross-site and forged-origin requests are blocked", () => {
  assert.ok(!sameOrigin(mk("POST", { origin: "https://evil.example" })));
  assert.ok(!sameOrigin(mk("POST", { origin: "https://evil.example", "sec-fetch-site": "same-origin" })), "a mismatching Origin always fails");
  assert.ok(!sameOrigin(mk("POST", { "sec-fetch-site": "cross-site" })));
  assert.ok(!sameOrigin(mk("POST", { "sec-fetch-site": "same-site" })));
  assert.ok(!sameOrigin(mk("POST", { referer: "https://evil.example/page" })));
  assert.ok(!sameOrigin(mk("POST", {})), "no browser signals on a state-changing request");
  assert.ok(!sameOrigin(mk("POST", { origin: "not a url" })));
});

test("safe methods without signals are allowed; forwarded host is honoured", () => {
  assert.ok(sameOrigin(mk("GET", {})));
  assert.ok(sameOrigin(mk("GET", { "sec-fetch-site": "none" })));
  assert.ok(!sameOrigin(mk("POST", { "sec-fetch-site": "none" })));
  assert.ok(sameOrigin(mk("POST", { origin: "https://public.example", "x-forwarded-host": "public.example" })));
});
