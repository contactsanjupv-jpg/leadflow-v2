// An evidence package is an audit artifact: a record the reader cannot use must be FLAGGED, never silently
// dropped, or an incomplete list would look complete. These tests pin that for every list the page shows.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import EvidencePackage from "../components/EvidencePackage";
import { buildEvidenceView, hasListIssues, type EvidenceView, type ListIssue } from "./evidence";
import { CT_2026_0915_GET } from "./ct-2026-0915.fixture";
import { sampleContent } from "./evidence.fixture";

const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));
const view = (raw: unknown): EvidenceView => { const r = buildEvidenceView(raw); assert.equal(r.ok, true); return (r as Extract<typeof r, { ok: true }>).view; };
const html = (raw: unknown) => renderToStaticMarkup(createElement(EvidencePackage, { evidence: raw }));
const text = (h: string) => h.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ");
const JUNK = [null, "junk", 7, [1, 2], true];   // five malformed entries for lists of records
const TRACE_JUNK = [null, 7, [1, 2], true, { a: 1 }]; // a string IS a valid trace step, so the trace gets non-strings

interface ListCase {
  name: string; testid: string; what: string; junk?: unknown[];
  /** the object that owns the list, inside a cloned package */
  owner: (p: any) => any[]; key: string;
  issue: (v: EvidenceView) => ListIssue | null;
  rows: (v: EvidenceView) => unknown[] | null;
  /** a rendered phrase that appears ONLY when the list is silently treated as not recorded */
  notRecorded: string;
}
const CASES: ListCase[] = [
  { name: "invoice lines", testid: "invoice-lines", what: "invoice line items", owner: (p) => [p.content.invoice], key: "lines", issue: (v) => v.issues.invoiceLines, rows: (v) => v.invoice.lines, notRecorded: "Invoice line items are not recorded" },
  { name: "authorized fees", testid: "authorized-fees", what: "authorized fees", owner: (p) => [p.content.applicable_rates[0], p.content.contract_rate], key: "authorized_fees", issue: (v) => v.rates.applicable![0].feesIssue, rows: (v) => v.rates.applicable![0].fees, notRecorded: "Authorized fees are not recorded" },
  { name: "weight tickets", testid: "weight-tickets", what: "weight tickets", owner: (p) => [p.content], key: "weight_tickets", issue: (v) => v.issues.weightTickets, rows: (v) => (v.manifest as any).tickets, notRecorded: "Weight tickets are not recorded" },
  { name: "applicable rates", testid: "applicable-rates", what: "applicable rates", owner: (p) => [p.content], key: "applicable_rates", issue: (v) => v.issues.applicableRates, rows: (v) => v.rates.applicable, notRecorded: "Contract rates are not recorded" },
  { name: "priced lines", testid: "priced-lines", what: "priced lines", owner: (p) => [p.content], key: "priced_lines", issue: (v) => v.issues.pricedLines, rows: (v) => v.calculations.pricedLines, notRecorded: "Priced lines are not recorded" },
  { name: "source documents", testid: "source-documents", what: "source documents", owner: (p) => [p.content], key: "source_documents", issue: (v) => v.issues.sourceDocuments, rows: () => [], notRecorded: "No source documents are referenced" },
  { name: "calculation trace", junk: TRACE_JUNK, testid: "trace", what: "calculation trace steps", owner: (p) => [p.content.reconciliation], key: "calculation_trace", issue: (v) => v.issues.trace, rows: (v) => v.trace, notRecorded: "The calculation trace is not recorded" },
];

/** Break one list in a cloned CT-2026-0915 package. */
function broken(c: ListCase, mutate: (current: unknown) => unknown): any {
  const p = clone(CT_2026_0915_GET) as any;
  for (const o of c.owner(p)) o[c.key] = mutate(o[c.key]);
  return p;
}

for (const c of CASES) {
  test(`${c.name}: malformed entries are flagged, valid entries are kept, nothing is silently dropped`, () => {
    const junk = c.junk ?? JUNK;
    const original = (c.owner(clone(CT_2026_0915_GET))[0][c.key] as unknown[]);
    const p = broken(c, (cur) => [...(cur as unknown[]), ...junk]);
    const v = view(p);
    const issue = c.issue(v);
    assert.deepEqual(issue, { kind: "partial", skipped: junk.length, total: original.length + junk.length }, "every malformed entry is counted");
    assert.equal(hasListIssues(v), true);
    if (c.name !== "source documents") assert.equal(c.rows(v)!.length, original.length, "the valid entries are still shown");
    const h = html(p);
    assert.ok(h.includes(`data-testid="evidence-list-issue-${c.testid}"`), "the section carries a visible alert");
    assert.ok(text(h).includes(`Incomplete: ${junk.length} of ${original.length + junk.length} ${c.what} could not be read and are not shown`), text(h).slice(0, 0) + c.what);
    assert.ok(h.includes('data-testid="evidence-incomplete-banner"'), "the package as a whole is flagged incomplete");
    assert.ok(h.includes('role="alert"'));
  });

  test(`${c.name}: a value that is not a list is reported as unreadable, not as "not recorded"`, () => {
    for (const bad of ["oops", { a: 1 }, 5, true]) {
      const p = broken(c, () => bad);
      const v = view(p);
      assert.deepEqual(c.issue(v), { kind: "unreadable" }, JSON.stringify(bad));
      const h = html(p);
      assert.ok(h.includes(`data-testid="evidence-list-issue-${c.testid}"`));
      assert.ok(text(h).includes(`the ${c.what} could not be read (unexpected format), so none are shown`));
      assert.ok(!text(h).includes(c.notRecorded), `must not claim "${c.notRecorded}" for an unreadable list`);
    }
  });

  test(`${c.name}: missing, recorded null and empty are NOT malformed and raise no alert`, () => {
    for (const mutate of [() => undefined, () => null, () => []]) {
      const p = broken(c, mutate);
      for (const o of c.owner(p)) if (o[c.key] === undefined) delete o[c.key];
      const v = view(p);
      assert.equal(c.issue(v), null);
      assert.equal(hasListIssues(v), false);
      const h = html(p);
      assert.ok(!h.includes("evidence-list-issue") && !h.includes("evidence-incomplete-banner"));
    }
  });
}

test("a single malformed entry reads in the singular", () => {
  const p = broken(CASES[0], (cur) => [...(cur as unknown[]), "junk"]);
  assert.ok(text(html(p)).includes("Incomplete: 1 of 2 invoice line items could not be read and is not shown"));
});

test("source documents without a usable id cannot be shown, so they are flagged rather than skipped", () => {
  const sd = CASES[5];
  const p = broken(sd, () => [{ original_filename: "no-id.pdf" }, { id: "", original_filename: "blank-id.pdf" }, { id: "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a66", original_filename: "ok.pdf", document_type: "OTHER", sha256: "c".repeat(64) }]);
  const v = view(p);
  assert.deepEqual(v.issues.sourceDocuments, { kind: "partial", skipped: 2, total: 3 });
  assert.ok(v.documents.some((d) => d.doc?.id === "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a66"), "the usable document is still listed");
  assert.ok(text(html(p)).includes("Incomplete: 2 of 3 source documents could not be read"));
});

test("calculation trace: non-string steps are flagged, string steps are kept in order", () => {
  const tr = CASES[6];
  const p = broken(tr, () => ["step one", 42, "step two", null]);
  const v = view(p);
  assert.deepEqual(v.trace, ["step one", "step two"]);
  assert.deepEqual(v.issues.trace, { kind: "partial", skipped: 2, total: 4 });
});

test("weight tickets are flagged even when no manifest is linked", () => {
  const p = clone(CT_2026_0915_GET) as any;
  p.content.manifest = null; p.content.weight_tickets = [null, "x"];
  const v = view(p);
  assert.deepEqual(v.issues.weightTickets, { kind: "partial", skipped: 2, total: 2 });
  assert.ok(html(p).includes('data-testid="evidence-list-issue-weight-tickets"'));
});

test("authorized fees are flagged on the parent contract rate too, not only on applicable rates", () => {
  const p = clone(CT_2026_0915_GET) as any;
  delete p.content.applicable_rates; p.content.contract_rate.authorized_fees = [1, { fee_code: "X", amount: "1.0000", charge_type: null }];
  const v = view(p);
  assert.deepEqual(v.rates.parent!.feesIssue, { kind: "partial", skipped: 1, total: 2 });
  assert.equal(v.rates.parent!.fees!.length, 1);
  assert.ok(html(p).includes('data-testid="evidence-list-issue-authorized-fees"'));
});

test("conservation: for every list, readable rows + flagged entries always equals the entries received", () => {
  for (const mix of [[], [null], ["a", { id: "x" }], [{ id: "x" }, { id: "y" }], [1, 2, 3, { id: "z" }]]) {
    const p = clone(CT_2026_0915_GET) as any; p.content.invoice.lines = mix;
    const v = view(p);
    const flagged = v.issues.invoiceLines?.kind === "partial" ? v.issues.invoiceLines.skipped : 0;
    assert.equal(v.invoice.lines!.length + flagged, mix.length, JSON.stringify(mix));
  }
});

test("valid packages are untouched: no issues, no alerts, no banner (backend-generated and synthetic)", () => {
  for (const pkg of [CT_2026_0915_GET, { id: "p", discrepancy_id: "d-1", content: sampleContent() }]) {
    const v = view(pkg);
    assert.ok(Object.values(v.issues).every((i) => i === null));
    assert.equal(hasListIssues(v), false);
    const h = html(pkg);
    assert.ok(!h.includes("evidence-list-issue") && !h.includes("evidence-incomplete-banner") && !h.includes("Incomplete:"));
  }
});

test("the raw JSON toggle is still available when a package is incomplete", () => {
  const p = broken(CASES[0], (cur) => [...(cur as unknown[]), null]);
  assert.ok(html(p).includes('data-testid="evidence-raw-toggle"'));
});

// ---- parent contract rate versus an unreadable applicable-rate list -------------------------------
// contract_rate is the PARENT rate (set only when exactly one rate applied). When the applicable-rate list cannot be
// read, showing the parent as "Contract rate" beside an alert saying no rates are shown would let a reviewer read it as the
// rate that applied. It must be shown only as an explicitly unconfirmed parent.
const PARENT_OK = '<h4>Contract rate</h4>';
const withApplicable = (value: unknown) => { const p = clone(CT_2026_0915_GET) as any; if (value === undefined) delete p.content.applicable_rates; else p.content.applicable_rates = value; return p; };

test("unreadable applicable rates: the parent rate is shown only as unconfirmed, never as the rate that applied", () => {
  for (const bad of ["oops", { a: 1 }, 5, true]) {
    const p = withApplicable(bad);
    const v = view(p);
    assert.equal(v.rates.parentUnconfirmed, true, JSON.stringify(bad));
    const h = html(p);
    assert.ok(!h.includes(PARENT_OK), "must not be headed as the plain contract rate");
    assert.ok(!h.includes("Applicable rate 1"), "must not be presented as an applicable rate");
    assert.ok(h.includes('data-testid="evidence-parent-rate-unconfirmed"'));
    assert.ok(text(h).includes("Parent contract rate (not confirmed as an applicable rate)"));
    assert.ok(text(h).includes("It is NOT confirmed as the rate that applied"));
    assert.ok(h.includes('data-testid="evidence-list-issue-applicable-rates"') && h.includes('data-testid="evidence-incomplete-banner"'));
    // recorded evidence is not hidden: the parent's own facts are still there
    assert.ok(text(h).includes("$450.00"));
  }
});

test("applicable rates where no entry is readable: same unconfirmed treatment", () => {
  const p = withApplicable([null, "junk"]);
  const v = view(p);
  assert.deepEqual(v.issues.applicableRates, { kind: "partial", skipped: 2, total: 2 });
  assert.equal(v.rates.parentUnconfirmed, true);
  const h = html(p);
  assert.ok(!h.includes(PARENT_OK) && h.includes('data-testid="evidence-parent-rate-unconfirmed"'));
});

test("the contract document is labelled as the parent's when it stands in for an unreadable list", () => {
  const p = withApplicable("oops");
  p.content.contract_rate.contract_source_document_id = "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a67";
  p.content.source_documents = [{ id: "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a67", document_type: "CONTRACT", original_filename: "contract.pdf", sha256: "d".repeat(64), created_at: "t" }];
  const role = view(p).documents.find((d) => d.doc?.id === "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a67")!.role;
  assert.ok(role.includes("parent rate, not confirmed as applicable"), role);
  // for a genuinely absent list the label stays plain
  const q = withApplicable(undefined); q.content.contract_rate.contract_source_document_id = "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a67"; q.content.source_documents = p.content.source_documents;
  assert.ok(!view(q).documents.find((d) => d.doc?.id === "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a67")!.role.includes("not confirmed"));
});

test("readable applicable rates beside malformed ones: the parent is NOT shown or marked, the readable rate is", () => {
  const p = withApplicable([...(clone(CT_2026_0915_GET) as any).content.applicable_rates, null]);
  const v = view(p);
  assert.equal(v.rates.parentUnconfirmed, false);
  const h = html(p);
  assert.ok(h.includes("Applicable rate 1 of 1") && !h.includes('data-testid="evidence-parent-rate-unconfirmed"') && !h.includes(PARENT_OK));
});

test("a genuinely absent, null or empty applicable-rate list still shows the parent contract rate normally", () => {
  for (const value of [undefined, null, []]) {
    const p = withApplicable(value);
    const v = view(p);
    assert.equal(v.rates.parentUnconfirmed, false, String(value));
    assert.equal(v.issues.applicableRates, null);
    const h = html(p);
    assert.ok(h.includes(PARENT_OK), `parent shown as the contract rate for ${JSON.stringify(value)}`);
    assert.ok(!h.includes("evidence-parent-rate-unconfirmed") && !h.includes("evidence-list-issue") && !h.includes("evidence-incomplete-banner"));
  }
});

test("unreadable applicable rates with no parent rate: only the alert, no rate block", () => {
  const p = withApplicable("oops"); p.content.contract_rate = null;
  const v = view(p);
  assert.equal(v.rates.parentUnconfirmed, false);
  const h = html(p);
  assert.ok(!h.includes("evidence-parent-rate-unconfirmed") && !h.includes(PARENT_OK) && h.includes('data-testid="evidence-list-issue-applicable-rates"'));
  assert.ok(!text(h).includes("Contract rates are not recorded"));
});

// ---- the parent's contract document label follows the same rule as the parent rate block -----------------
const STANDS_IN = "parent rate, not confirmed as applicable";
function withContractDoc(applicable: unknown) {
  const p = withApplicable(applicable);
  p.content.contract_rate.contract_source_document_id = "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a67";
  p.content.source_documents = [{ id: "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a67", document_type: "CONTRACT", original_filename: "contract.pdf", sha256: "d".repeat(64), created_at: "t" }];
  return p;
}
const rolesFor = (p: unknown) => view(p).documents.filter((d) => d.doc?.id === "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a67").map((d) => d.role);

test("applicable_rates with every entry malformed: the parent's contract document says it stands in, not 'Supporting document'", () => {
  const p = withContractDoc([null, "junk"]);
  const v = view(p);
  assert.equal(v.rates.parentUnconfirmed, true);
  assert.deepEqual(v.issues.applicableRates, { kind: "partial", skipped: 2, total: 2 });
  const roles = rolesFor(p);
  assert.equal(roles.length, 1, "the document is listed exactly once");
  assert.ok(roles[0].startsWith("Contract — Example Disposal Agreement") && roles[0].includes(STANDS_IN), roles[0]);
  assert.ok(text(html(p)).includes(STANDS_IN), "and the label is visible on the page");
});

test("the parent-rate block and the document label can never disagree", () => {
  for (const value of ["oops", { a: 1 }, 5, true, [null, "junk"], [1, 2, 3], [[]]]) {
    const p = withContractDoc(value);
    const v = view(p);
    assert.equal(v.rates.parentUnconfirmed, true, JSON.stringify(value));
    assert.ok(rolesFor(p).every((r) => r.includes(STANDS_IN)), JSON.stringify(value) + " -> " + rolesFor(p));
  }
  for (const value of [undefined, null, [], [...(clone(CT_2026_0915_GET) as any).content.applicable_rates, null]]) {
    const p = withContractDoc(value);
    const v = view(p);
    assert.equal(v.rates.parentUnconfirmed, false, JSON.stringify(value));
    assert.ok(rolesFor(p).every((r) => !r.includes(STANDS_IN)), JSON.stringify(value) + " -> " + rolesFor(p));
  }
});

test("existing labels are unchanged for missing, null, empty and mixed applicable-rate lists", () => {
  assert.deepEqual(rolesFor(withContractDoc(undefined)), ["Contract — Example Disposal Agreement"]);
  assert.deepEqual(rolesFor(withContractDoc(null)), ["Contract — Example Disposal Agreement"]);
  assert.deepEqual(rolesFor(withContractDoc([])), ["Supporting document"], "an explicitly empty list keeps its existing behaviour");
  // mixed: the readable rate's own contract document is labelled plainly, with no stand-in wording
  const mixed = withContractDoc([{ ...(clone(CT_2026_0915_GET) as any).content.applicable_rates[0], contract_source_document_id: "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a67" }, null]);
  assert.deepEqual(rolesFor(mixed), ["Contract — Example Disposal Agreement"]);
});

test("an unreadable non-array value keeps its stand-in label", () => {
  for (const bad of ["oops", { a: 1 }, 5, true]) assert.ok(rolesFor(withContractDoc(bad))[0].includes(STANDS_IN), JSON.stringify(bad));
});
