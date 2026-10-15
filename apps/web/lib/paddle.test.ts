import test from "node:test";
import assert from "node:assert/strict";
import { buildCheckoutOptions, isEntitledStatus } from "./paddle.ts";

const cfg = { configured: true, environment: "sandbox" as const, token: "test_x", priceId: "pri_123" };
const ctx = { organization_id: "11111111-1111-1111-1111-111111111111", organization_sig: "abc123", email: "owner@example.com" };

test("checkout carries the organization id AND its backend signature in custom data", () => {
  const o = buildCheckoutOptions(cfg, ctx);
  assert.deepEqual(o.items, [{ priceId: "pri_123", quantity: 1 }]);
  assert.equal(o.customData.organization_id, ctx.organization_id);
  assert.equal(o.customData.organization_sig, "abc123");
  assert.equal(o.customer.email, "owner@example.com");
});

test("checkout refuses to open when billing is not configured", () => {
  assert.throws(() => buildCheckoutOptions({ configured: false }, ctx), /not configured/);
  assert.throws(() => buildCheckoutOptions({ configured: true, token: "t" }, ctx), /not configured/);
});

test("entitlement states mirror the backend rule", () => {
  for (const s of ["active", "trialing", "past_due"]) assert.ok(isEntitledStatus(s), s);
  for (const s of ["paused", "canceled", "NONE", "", null, undefined]) assert.ok(!isEntitledStatus(s as any), String(s));
});
