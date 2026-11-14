// Malformed scalar values and malformed nested objects are FLAGGED, never shown as "not recorded".
import { test } from "node:test";
import assert from "node:assert/strict";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import EvidencePackage from "../components/EvidencePackage";
import { buildEvidenceView, cell, countUnreadable, hasEvidenceIssues, type EvidenceView, type Field } from "./evidence";
import { CT_2026_0915_GET } from "./ct-2026-0915.fixture";
import { sampleContent } from "./evidence.fixture";

const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));
const view = (raw: unknown): EvidenceView => { const r = buildEvidenceView(raw); assert.equal(r.ok, true); return (r as Extract<typeof r, { ok: true }>).view; };
const html = (raw: unknown) => renderToStaticMarkup(createElement(EvidencePackage, { evidence: raw }));
const text = (h: string) => h.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ");
const ct = () => clone(CT_2026_0915_GET) as any;
const f = (fs: Field[], label: string) => { const x = fs.find((y) => y.label === label); assert.ok(x, label); return x!; };
const UNREADABLE_TESTID = 'data-testid="evidence-unreadable-value"';

// ---- cell(): the four states -----------------------------------------------------------------------
test("cell: a missing key stays absent", () => {
  assert.equal(cell({ other: 1 }, "k").state, "absent");
  assert.equal(cell(null, "k").state, "absent");
});
test("cell: an explicit null stays null, and keeps its custom wording", () => {
  const c = cell({ k: null }, "k", { nullText: "No end date (open-ended)" });
  assert.equal(c.state, "null"); assert.equal(c.nullText, "No end date (open-ended)");
});
test("cell: valid values are unchanged", () => {
  assert.deepEqual([cell({ k: "abc" }, "k").state, cell({ k: "abc" }, "k").value], ["value", "abc"]);
  assert.equal(cell({ k: "4500.0000" }, "k", { money: true }).value, "$4,500.00");
  assert.equal(cell({ k: "-12.50" }, "k", { money: true }).value, "-$12.50");
  assert.equal(cell({ k: 2 }, "k", { allowNumber: true }).value, "2", "an integer count may be a JSON number where allowed");
  assert.equal(cell({ k: "2026-01-01" }, "k").value, "2026-01-01");
});
test("cell: wrong-typed scalar values are unreadable, not absent", () => {
  for (const bad of [{ a: 1 }, [1], true, false, "", "   "]) assert.equal(cell({ k: bad }, "k").state, "unreadable", JSON.stringify(bad));
  assert.equal(cell({ k: 5 }, "k").state, "unreadable", "a number where text is expected (numbers are accepted only where allowNumber is set)");
  for (const bad of [4500, "abc", "4,500.00", "45.0.0", "12e3", "$5", " 5", { v: "5" }, true]) assert.equal(cell({ k: bad }, "k", { money: true }).state, "unreadable", "money: " + JSON.stringify(bad));
  assert.equal(cell({ k: "x" }, "k", { format: () => null }).state, "unreadable");
});
test("cell: every field of an unreadable parent object is unreadable", () => {
  assert.equal(cell("unreadable", "anything").state, "unreadable");
});

// ---- scalar fields inside a valid package ------------------------------------------------------------
test("a wrong-typed field is shown as unreadable beside its valid neighbours, with the package flagged", () => {
  const p = ct();
  p.content.reconciliation.expected_amount = { oops: true };      // wrong type
  p.content.invoice.invoice_date = "";                              // blank
  p.content.applicable_rates[0].unit_rate = 450;                    // a float where an exact decimal string is required
  const v = view(p);
  assert.equal(f(v.calculations.fields, "Expected amount").state, "unreadable");
  assert.equal(f(v.invoice.fields, "Invoice date").state, "unreadable");
  assert.equal(f(v.rates.applicable![0].fields, "Rate").state, "unreadable");
  assert.equal(f(v.calculations.fields, "Invoiced amount").value, "$5,200.00", "neighbours are untouched");
  assert.ok(countUnreadable(v) >= 3 && hasEvidenceIssues(v));
  const h = html(p);
  assert.ok(h.includes(UNREADABLE_TESTID) && h.includes('data-testid="evidence-incomplete-banner"'));
  assert.ok(text(h).includes("Unreadable value (unexpected format)"));
});

test("an unreadable value never reads as 'Not recorded' in its place", () => {
  const p = ct(); p.content.reconciliation.expected_amount = ["x"];
  const h = html(p);
  const calc = h.slice(h.indexOf('data-testid="evidence-calculations"'), h.indexOf('data-testid="evidence-discrepancy"'));
  const expectedRow = calc.slice(calc.indexOf("Expected amount"), calc.indexOf("Invoiced amount"));
  assert.ok(expectedRow.includes("Unreadable value") && !expectedRow.includes("Not recorded"), expectedRow);
});

test("explicit null and missing keys still read as they did (open-ended end date, Not recorded)", () => {
  const nullEnd = ct();                                // the real package: effective_to is an explicit null
  assert.ok(text(html(nullEnd)).includes("No end date (open-ended)"));
  const missing = ct(); delete missing.content.applicable_rates[0].effective_to;
  const h = html(missing);
  assert.ok(!h.includes("open-ended") && !h.includes(UNREADABLE_TESTID) && text(h).includes("Not recorded in this package"));
  const malformed = ct(); malformed.content.applicable_rates[0].effective_to = "";
  const hm = html(malformed);
  assert.ok(!hm.includes("open-ended") && hm.includes(UNREADABLE_TESTID), "a blank end date is malformed, never open-ended");
});

test("priced-line ids, fee code, basis and source-document references are checked too", () => {
  const p = ct();
  p.content.applicable_rates[0].priced_line_ids = "oops";
  p.content.applicable_rates[0].authorized_fees = [{ fee_code: { x: 1 }, description: null, amount: "1.0000", charge_type: "CAP" }];
  p.content.applicable_rates_basis = { x: 1 };
  p.content.invoice.source_document_id = { x: 1 };
  p.content.source_documents = [{ id: "doc-1", document_type: "INVOICE", original_filename: { x: 1 }, sha256: "a".repeat(64), created_at: "t" }];
  p.content.manifest.source_document_id = "doc-1";
  const v = view(p);
  assert.equal(v.rates.applicable![0].pricedLines.state, "unreadable");
  assert.equal(v.rates.applicable![0].fees![0].code.state, "unreadable");
  assert.equal(v.rates.basisUnreadable, true);
  assert.equal(v.documents.find((d) => d.role === "Invoice")!.state, "unreadable");
  assert.equal(v.documents.find((d) => d.doc?.id === "doc-1")!.doc!.detailsUnreadable, true);
  const h = html(p);
  assert.ok(h.includes('data-testid="evidence-basis-unreadable"') && text(h).includes("Unreadable source-document reference") && text(h).includes("Some document details could not be read"));
});

// ---- non-object manifest ----------------------------------------------------------------------------
test("a present manifest that is not an object is flagged, not treated as absent", () => {
  for (const bad of ["MAN-1", 5, true, [1, 2], []]) {
    const p = ct(); p.content.manifest = bad;
    const v = view(p);
    assert.equal(v.manifest.state, "unreadable", JSON.stringify(bad));
    assert.equal(v.objects.manifest, true);
    const h = html(p);
    assert.ok(h.includes('data-testid="evidence-object-issue-manifest"') && h.includes('data-testid="evidence-incomplete-banner"'));
    assert.ok(!text(h).includes("The manifest is not recorded in this package") && !text(h).includes("No manifest is linked"));
  }
});
test("manifest: a missing key and an explicit null are unchanged", () => {
  const a = ct(); delete a.content.manifest; assert.equal(view(a).manifest.state, "absent"); assert.ok(text(html(a)).includes("The manifest is not recorded in this package"));
  const b = ct(); b.content.manifest = null; assert.equal(view(b).manifest.state, "none"); assert.ok(text(html(b)).includes("No manifest is linked to this reconciliation"));
  for (const p of [a, b]) { const h = html(p); assert.ok(!h.includes("evidence-object-issue") && !h.includes("evidence-incomplete-banner")); }
});

// ---- non-object parent contract rate and the other nested objects -------------------------------------
test("a present contract_rate that is not an object is flagged, not treated as no parent rate", () => {
  for (const bad of ["rate-1", 5, true, [1], []]) {
    const p = ct(); delete p.content.applicable_rates; p.content.contract_rate = bad;
    const v = view(p);
    assert.equal(v.rates.parentUnreadable, true, JSON.stringify(bad));
    assert.equal(v.objects.contractRate, true);
    const h = html(p);
    assert.ok(h.includes('data-testid="evidence-object-issue-contract-rate"') && h.includes('data-testid="evidence-incomplete-banner"'));
    assert.ok(!text(h).includes("Contract rates are not recorded"), "must not claim the rate is simply not recorded");
  }
});
test("contract_rate: explicit null (multi-rate) and a missing key stay unflagged", () => {
  const a = ct(); a.content.contract_rate = null; assert.equal(view(a).rates.parentUnreadable, false);
  const b = ct(); delete b.content.contract_rate; assert.equal(view(b).rates.parentUnreadable, false);
  for (const p of [a, b]) assert.ok(!html(p).includes("evidence-object-issue"));
});
test("discrepancy, reconciliation and invoice that are present but not objects are flagged with unreadable fields", () => {
  for (const key of ["discrepancy", "reconciliation", "invoice"] as const) {
    const p = ct(); p.content[key] = "garbage";
    const v = view(p);
    assert.equal(v.objects[key], true, key);
    const h = html(p);
    assert.ok(h.includes(`data-testid="evidence-object-issue-${key}"`) && h.includes(UNREADABLE_TESTID) && h.includes('data-testid="evidence-incomplete-banner"'), key);
  }
  // the lists that live inside a malformed parent object are reported as unreadable too, not as empty or missing
  const r = ct(); r.content.reconciliation = 7; assert.deepEqual(view(r).issues.trace, { kind: "unreadable" });
  const i = ct(); i.content.invoice = 7; assert.deepEqual(view(i).issues.invoiceLines, { kind: "unreadable" });
});
test("nested objects: missing and explicit null are not flagged", () => {
  for (const key of ["discrepancy", "reconciliation", "invoice"]) {
    const a = ct(); delete a.content[key]; const b = ct(); b.content[key] = null;
    for (const p of [a, b]) { const v = view(p); assert.equal(v.objects[key as "invoice"], false); assert.ok(!html(p).includes("evidence-object-issue")); }
  }
});

// ---- valid packages render exactly as before -----------------------------------------------------------
test("valid packages: no unreadable marker, no object alert, no banner (backend-generated and synthetic)", () => {
  for (const pkg of [CT_2026_0915_GET, { id: "p", discrepancy_id: "d-1", content: sampleContent() }]) {
    const v = view(pkg);
    assert.equal(countUnreadable(v), 0);
    assert.equal(hasEvidenceIssues(v), false);
    assert.ok(Object.values(v.objects).every((x) => x === false) && !v.rates.parentUnreadable && !v.rates.basisUnreadable);
    const h = html(pkg);
    for (const marker of [UNREADABLE_TESTID, "evidence-object-issue", "evidence-incomplete-banner", "evidence-basis-unreadable", "evidence-list-issue"]) assert.ok(!h.includes(marker), marker);
    assert.ok(!text(h).includes("Unreadable"));
  }
});

// ---- document metadata: only non-empty STRINGS are readable (a number is not converted to text) ----------
const META_KEYS = ["original_filename", "document_type", "sha256"] as const;
function withDoc(meta: Record<string, unknown>, how: "linked" | "listed" = "linked") {
  const p = ct();
  if (how === "linked") p.content.invoice.source_document_id = "doc-1";
  else p.content.invoice.source_document_id = null;
  p.content.source_documents = [{ id: "doc-1", document_type: "INVOICE", original_filename: "invoice.pdf", sha256: "a".repeat(64), created_at: "t", ...meta }];
  return p;
}
const docOf = (p: unknown) => view(p).documents.find((d) => d.doc?.id === "doc-1")!.doc!;

test("document metadata: numeric, boolean, object, array and blank values are unreadable and never displayed as text", () => {
  for (const key of META_KEYS) {
    for (const bad of [987654, 0, true, false, { a: 1 }, [1, 2], "", "   "]) {
      for (const how of ["linked", "listed"] as const) {
        const p = withDoc({ [key]: bad }, how);
        const d = docOf(p);
        assert.equal(d.detailsUnreadable, true, `${key}=${JSON.stringify(bad)} (${how})`);
        const shown = key === "original_filename" ? d.filename : key === "document_type" ? d.documentType : d.sha256;
        assert.equal(shown, null, `${key}=${JSON.stringify(bad)} must not be turned into text`);
        const v = view(p);
        assert.ok(countUnreadable(v) >= 1 && hasEvidenceIssues(v));
        const h = html(p);
        assert.ok(text(h).includes("Some document details could not be read") && h.includes('data-testid="evidence-incomplete-banner"'), `${key}=${JSON.stringify(bad)}`);
      }
    }
  }
});

test("document metadata: a numeric value is not rendered as if it were a filename, type or hash", () => {
  const p = withDoc({ original_filename: 987654, document_type: 555111, sha256: 424242 });
  const h = text(html(p));
  for (const n of ["987654", "555111", "424242"]) assert.ok(!h.includes(n), n);
  assert.ok(h.includes("name not included in this package"), "no filename is shown, and the document is still downloadable");
  assert.ok(html(p).includes('href="/api/proxy/documents/doc-1/download"'));
});

test("document metadata: missing keys and explicit null are unchanged (no value, not flagged)", () => {
  for (const key of META_KEYS) {
    const missing = withDoc({}); delete missing.content.source_documents[0][key];
    const nulled = withDoc({ [key]: null });
    for (const p of [missing, nulled]) {
      const d = docOf(p);
      assert.equal(d.detailsUnreadable, false, key);
      assert.equal(key === "original_filename" ? d.filename : key === "document_type" ? d.documentType : d.sha256, null);
      assert.ok(!html(p).includes("Some document details could not be read"));
    }
  }
});

test("document metadata: valid string values are unchanged and raise no warning", () => {
  const p = withDoc({});
  const d = docOf(p);
  assert.deepEqual([d.filename, d.documentType, d.sha256, d.detailsUnreadable], ["invoice.pdf", "INVOICE", "a".repeat(64), false]);
  const h = html(p);
  assert.ok(h.includes("invoice.pdf") && !h.includes("Some document details could not be read") && !h.includes('data-testid="evidence-incomplete-banner"'));
});

test("document metadata: one bad field does not hide the readable ones", () => {
  const d = docOf(withDoc({ original_filename: 123 }));
  assert.equal(d.filename, null);
  assert.equal(d.documentType, "INVOICE");
  assert.equal(d.sha256, "a".repeat(64));
  assert.equal(d.detailsUnreadable, true);
});
