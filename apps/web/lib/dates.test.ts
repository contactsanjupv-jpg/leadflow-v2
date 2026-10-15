import test from "node:test";
import assert from "node:assert/strict";
import { isPlausibleIsoDate, MAX_DATE, MIN_DATE } from "./dates.ts";

test("both readings of 09/12/2026 are accepted exactly as the date control submits them", () => {
  assert.ok(isPlausibleIsoDate("2026-09-12")); // month/day locales
  assert.ok(isPlausibleIsoDate("2026-12-09")); // day/month locales
});

test("partial or implausible years are rejected, including the exact value from the bug report", () => {
  for (const bad of ["0206-12-09", "0026-01-01", "0002-09-12", "1899-12-31", "2101-01-01", "9999-12-31"]) assert.ok(!isPlausibleIsoDate(bad), bad);
});

test("boundaries are inclusive and the exported bounds are valid", () => {
  assert.ok(isPlausibleIsoDate("1900-01-01") && isPlausibleIsoDate("2100-12-31"));
  assert.ok(isPlausibleIsoDate(MIN_DATE) && isPlausibleIsoDate(MAX_DATE));
});

test("malformed and impossible calendar dates are rejected", () => {
  for (const bad of ["", "2026-9-12", "12/09/2026", "2026-02-30", "2025-02-29", "2026-13-01", "2026-00-10", "abcd-ef-gh", "2026-09-12T00:00"]) assert.ok(!isPlausibleIsoDate(bad), bad);
  assert.ok(isPlausibleIsoDate("2024-02-29"));
});