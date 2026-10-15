import test from "node:test";
import assert from "node:assert/strict";
import { formatMoney, isValidDecimal } from "./money.ts";

test("formats exact decimal strings without floats", () => {
  assert.equal(formatMoney("5200.0000"), "$5,200.00");
  assert.equal(formatMoney("700.0000"), "$700.00");
  assert.equal(formatMoney("1234567.8900"), "$1,234,567.89");
  assert.equal(formatMoney("0.1250"), "$0.125");
  assert.equal(formatMoney("-12.5000"), "-$12.50");
  assert.equal(formatMoney("9999999999.9999"), "$9,999,999,999.9999");
});

test("handles missing values and leaves unparseable input alone", () => {
  assert.equal(formatMoney(null), "—");
  assert.equal(formatMoney(undefined), "—");
  assert.equal(formatMoney("abc"), "abc");
});

test("decimal validation matches the API's NUMERIC(14,4) contract", () => {
  for (const ok of ["0", "10", "450.25", "-3.5", "9999999999.9999"]) assert.ok(isValidDecimal(ok), ok);
  for (const bad of ["", "1.23456", "1e5", "12,5", "abc", "10000000000"]) assert.ok(!isValidDecimal(bad), bad);
});
