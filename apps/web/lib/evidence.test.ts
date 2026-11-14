import { test } from "node:test";
import assert from "node:assert/strict";
import { buildEvidenceView, cell, createLatestGuard, feeChargeLabel, selectEvidence, type EvidenceView, type Field } from "./evidence";
import { sampleContent } from "./evidence.fixture";
import { CT_2026_0915_GET, ct20260915Post } from "./ct-2026-0915.fixture";

function view(raw: unknown): EvidenceView {
  const r = buildEvidenceView(raw);
  assert.equal(r.ok, true, "expected a readable package");
  return (r as Extract<typeof r, { ok: true }>).view;
}
const f = (fs: Field[], label: string) => { const x = fs.find((y) => y.label === label); assert.ok(x, `no field ${label}`); return x!; };
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));
const ct = () => clone(CT_2026_0915_GET) as { content: any } & Record<string, unknown>;

// ---- the backend-generated CT-2026-0915 package -------------------------------------------------

test("CT-2026-0915: the real figures are shown exactly: $4,500 expected, $5,200 invoiced, $700 discrepancy", () => {
  for (const raw of [CT_2026_0915_GET, ct20260915Post()]) {
    const v = view(raw);
    assert.equal(f(v.calculations.fields, "Expected amount").value, "$4,500.00");
    assert.equal(f(v.calculations.fields, "Invoiced amount").value, "$5,200.00");
    assert.equal(f(v.calculations.fields, "Discrepancy (billed − expected)").value, "$700.00");
    assert.equal(f(v.summary, "Finding amount").value, "$700.00");
    assert.equal(f(v.summary, "Rule").value, "RULE_03_RATE_MISMATCH");
    assert.equal(f(v.summary, "Invoice").value, "CT-2026-0915");
    assert.equal(f(v.calculations.fields, "Pricing status").value, "COMPLETE");
    assert.equal(v.rates.basisLabel, "Recorded when this invoice was reconciled");
  }
});

test("CT-2026-0915: contract rate $450.00 versus invoice rate $520.00, taken from the package", () => {
  const v = view(CT_2026_0915_GET);
  assert.equal(v.rates.applicable!.length, 1);
  assert.equal(f(v.rates.applicable![0].fields, "Rate").value, "$450.00");
  assert.equal(v.invoice.lines![0].unitPrice.value, "$520.00");
  assert.equal(v.invoice.lines![0].lineTotal.value, "$5,200.00");
  assert.equal(v.calculations.pricedLines![0].expected.value, "$4,500.00");
  assert.equal(v.calculations.pricedLines![0].unitRate.value, "$450.00");
  assert.equal(v.trace!.length, 4);
});

test("CT-2026-0915: no weight tickets, no contract source document and no other source documents are recorded", () => {
  const v = view(CT_2026_0915_GET);
  assert.equal(v.manifest.state, "present");
  assert.deepEqual((v.manifest as any).tickets, [], "weight tickets are recorded as an empty list");
  const contract = v.documents.find((d) => d.role.startsWith("Contract"))!;
  assert.equal(contract.state, "none", "the contract source document is recorded as null, so none is linked");
  assert.equal(contract.doc, null);
  assert.ok(v.documents.every((d) => d.state === "none"), "no document is linked anywhere in this package");
  assert.equal(v.documents.some((d) => d.doc !== null), false, "no document is invented");
});

test("CT-2026-0915: effective_to is explicitly null, which the schema defines as an open-ended rate", () => {
  const e = f(view(CT_2026_0915_GET).rates.applicable![0].fields, "Effective to");
  assert.equal(e.state, "null");
  assert.equal(e.nullText, "No end date (open-ended)");
});

// ---- recorded null versus a missing key ---------------------------------------------------------

test("a missing effective_to is Not recorded; only an explicit null is open-ended", () => {
  const c = ct(); delete c.content.applicable_rates[0].effective_to; delete c.content.contract_rate.effective_to;
  const e = f(view(c).rates.applicable![0].fields, "Effective to");
  assert.equal(e.state, "absent");
  assert.equal(e.value, null);
  const withDate = ct(); withDate.content.applicable_rates[0].effective_to = "2026-12-31";
  assert.equal(f(view(withDate).rates.applicable![0].fields, "Effective to").value, "2026-12-31");
});

test("cell() keeps value, null, absent and unreadable apart", () => {
  assert.equal(cell({ a: "x" }, "a").state, "value");
  assert.equal(cell({ a: null }, "a").state, "null");
  assert.equal(cell({}, "a").state, "absent");
  assert.equal(cell(null, "a").state, "absent");
  // a key that IS present but holds something unusable is malformed, no longer "absent"
  assert.equal(cell({ a: "   " }, "a").state, "unreadable");
  assert.equal(cell({ a: { nested: 1 } }, "a").state, "unreadable");
  assert.equal(cell({ a: "5200.0000" }, "a", { money: true }).value, "$5,200.00");
});

test("source document links: linked, explicitly none, and not recorded are three different states", () => {
  const c = sampleContent() as any;
  const linked = view(c).documents.find((d) => d.role === "Invoice")!;
  assert.equal(linked.state, "linked"); assert.equal(linked.doc!.filename, "CT-2026-0915.pdf");
  c.invoice.source_document_id = null;
  assert.equal(view(c).documents.find((d) => d.role === "Invoice")!.state, "none");
  delete c.invoice.source_document_id;
  assert.equal(view(c).documents.find((d) => d.role === "Invoice")!.state, "absent");
});

test("a linked document the package does not describe gets no invented name or hash", () => {
  const m = view(sampleContent()).documents.find((d) => d.role === "Manifest")!;
  assert.equal(m.state, "linked");
  assert.equal(m.doc!.filename, null); assert.equal(m.doc!.sha256, null);
  assert.equal(m.doc!.href, "/api/proxy/documents/doc-manifest/download");
});

test("manifest: absent key, recorded null and a present record are distinct", () => {
  const c = ct();
  assert.equal(view(c).manifest.state, "present");
  c.content.manifest = null; assert.equal(view(c).manifest.state, "none");
  delete c.content.manifest; assert.equal(view(c).manifest.state, "absent");
});

test("weight tickets: a missing list is not the same as an empty list", () => {
  const c = ct(); delete c.content.weight_tickets;
  assert.equal((view(c).manifest as any).tickets, null);
  c.content.weight_tickets = [];
  assert.deepEqual((view(c).manifest as any).tickets, []);
});

test("lists: absent is null, empty is [] (invoice lines, priced lines, trace, fees, applicable rates)", () => {
  const c = ct();
  delete c.content.invoice.lines; delete c.content.priced_lines; delete c.content.reconciliation.calculation_trace; delete c.content.applicable_rates; delete c.content.contract_rate.authorized_fees;
  const a = view(c);
  assert.equal(a.invoice.lines, null); assert.equal(a.calculations.pricedLines, null); assert.equal(a.trace, null); assert.equal(a.rates.applicable, null); assert.equal(a.rates.parent!.fees, null);
  c.content.invoice.lines = []; c.content.priced_lines = []; c.content.reconciliation.calculation_trace = []; c.content.applicable_rates = []; c.content.contract_rate.authorized_fees = [];
  const e = view(c);
  assert.deepEqual(e.invoice.lines, []); assert.deepEqual(e.calculations.pricedLines, []); assert.deepEqual(e.trace, []); assert.deepEqual(e.rates.applicable, []); assert.deepEqual(e.rates.parent!.fees, []);
});

test("withheld amounts explain themselves from the recorded pricing status and are never filled in", () => {
  const c = ct(); c.content.reconciliation = { ...c.content.reconciliation, pricing_status: "PARTIAL", expected_amount: null, discrepancy_amount: null, partial_expected_amount: "4500.0000" };
  const v = view(c);
  assert.equal(f(v.calculations.fields, "Expected amount").state, "null");
  assert.equal(f(v.calculations.fields, "Expected amount").nullText, "Not computed: the invoice is only partially priced");
  assert.equal(f(v.calculations.fields, "Discrepancy (billed − expected)").value, null);
  assert.equal(f(v.calculations.fields, "Partial subtotal (priced lines only, not a bill total)").value, "$4,500.00");
  c.content.reconciliation = { ...c.content.reconciliation, pricing_status: "LEGACY_UNVERIFIED", partial_expected_amount: null, identified_finding_amount: null };
  assert.equal(f(view(c).calculations.fields, "Total of identified findings").nullText, "Not verified: legacy reconciliation");
});

test("the partial subtotal row is hidden when the package is complete and carries no such value", () => {
  assert.ok(!view(CT_2026_0915_GET).calculations.fields.some((x) => x.label.startsWith("Partial")));
});

test("fee authorization labels never turn a missing type into EXACT or CAP", () => {
  assert.equal(feeChargeLabel({ charge_type: "EXACT" }), "exact charge");
  assert.equal(feeChargeLabel({ charge_type: "CAP" }), "cap: maximum allowed");
  assert.equal(feeChargeLabel({ charge_type: null }), "unclassified: type not set");
  assert.equal(feeChargeLabel({ fee_code: "X" }), "type not recorded in this evidence package");
  assert.equal(feeChargeLabel({ charge_type: "weird" }), "unrecognised charge type");
  assert.deepEqual(view(sampleContent()).rates.applicable![0].fees!.map((x) => x.typeLabel), ["cap: maximum allowed", "unclassified: type not set"]);
  const nodesc = view(sampleContent()).rates.applicable![0].fees![1];
  assert.equal(nodesc.description.state, "null", "a recorded null description is not a missing one");
});

test("the bare content object is accepted as well as the API response wrapper", () => {
  assert.equal(buildEvidenceView((CT_2026_0915_GET as any).content).ok, true);
});

test("malformed or unavailable evidence is reported, never thrown", () => {
  for (const bad of [null, undefined, "oops", 42, [], {}, { content: "x" }, { content: [] }, { content: {} }]) {
    const r = buildEvidenceView(bad);
    assert.equal(r.ok, false, JSON.stringify(bad));
    assert.ok(!r.ok && r.reason.length > 0);
  }
  // Present but not objects/lists: flagged as unreadable (never "not recorded").
  const v = view({ discrepancy: "x", reconciliation: [], invoice: 5, source_documents: "nope", applicable_rates: {} });
  assert.equal(f(v.calculations.fields, "Expected amount").state, "unreadable");
  assert.equal(v.rates.applicable, null);
  assert.deepEqual(v.issues.applicableRates, { kind: "unreadable" });
});

// ---- showing an existing snapshot ---------------------------------------------------------------

test("selectEvidence shows the persisted snapshot once it has been read back, and the fresh package until then", () => {
  const generated = { discrepancy_id: "d-1", fresh: true };            // POST response: no created_at
  const persisted = { discrepancy_id: "d-1", created_at: "2026-09-16T10:00:00Z" }; // GET response
  // right after Generate, before the read-back: the package just generated
  assert.deepEqual(selectEvidence(generated, { "d-1": generated }, "d-1"), { evidence: generated, source: "generated" });
  // after the read-back: the persisted snapshot wins even though the generated response is still held
  assert.deepEqual(selectEvidence(generated, { "d-1": persisted }, "d-1"), { evidence: persisted, source: "stored" });
  // an OLDER persisted snapshot must not beat a fresh generate: the page replaces stored[id] with the POST response
  const older = { discrepancy_id: "d-1", created_at: "2026-01-01T00:00:00Z" };
  assert.equal(selectEvidence(generated, { "d-1": older }, "d-1")!.source, "stored", "pure rule: persisted wins, so the page must overwrite stored on POST");
  assert.equal(selectEvidence(generated, { "d-1": generated }, "d-1")!.source, "generated");
});

test("selectEvidence falls back to a stored snapshot, else nothing; other rows never leak in", () => {
  const stored = { "d-1": { discrepancy_id: "d-1", created_at: "t" } };
  assert.equal(selectEvidence({ discrepancy_id: "other" }, stored, "d-1")!.source, "stored");
  assert.equal(selectEvidence(null, stored, "d-1")!.source, "stored");
  assert.equal(selectEvidence(null, stored, "d-2"), null);
  assert.equal(selectEvidence({ discrepancy_id: "d-2" }, {}, "d-1"), null);
  assert.equal(selectEvidence(null, { "d-1": "garbage" }, "d-1"), null);
  assert.deepEqual(selectEvidence({ discrepancy_id: "d-1", fresh: 1 }, { "d-1": { discrepancy_id: "d-1" } }, "d-1")!.source, "generated");
});

test("latest-request-wins guard: only the newest request per key may apply its response", () => {
  const g = createLatestGuard();
  const first = g.begin("a"), second = g.begin("a");
  assert.equal(g.isCurrent("a", first), false, "an earlier request is stale once a newer one starts");
  assert.equal(g.isCurrent("a", second), true);
  const other = g.begin("b");
  assert.equal(g.isCurrent("a", second), true, "keys are independent: starting b does not cancel a");
  assert.equal(g.isCurrent("b", other), true);
  g.invalidate("a");
  assert.equal(g.isCurrent("a", second), false, "invalidate cancels everything in flight for that key");
  assert.equal(g.isCurrent("zzz", 1), false, "an unknown key is never current");
});

test("rapid row switching: responses arriving out of order cannot overwrite newer state", () => {
  // Simulates the page: state keyed by id, writes allowed only while the guard says the request is current.
  const g = createLatestGuard();
  const state: Record<string, string> = {};
  const apply = (id: string, token: number, value: string) => { if (g.isCurrent(id, token)) state[id] = value; };
  const aSlow = g.begin("a");            // open row A (slow response)
  const b = g.begin("b");                // switch to row B
  const aAgain = g.begin("a");           // switch back to A: a second request for A
  apply("b", b, "B-snapshot");
  apply("a", aAgain, "A-newest");        // newest A response arrives first
  apply("a", aSlow, "A-STALE");          // the earlier A response arrives last and is ignored
  assert.equal(state.b, "B-snapshot"); assert.equal(state.a, "A-newest");
  // A generate supersedes a read already in flight: a late 404/old snapshot must not clobber the fresh result.
  const readInFlight = g.begin("c");
  g.invalidate("c");                     // POST succeeded for c
  state.c = "POST-result";
  apply("c", readInFlight, "OLD-or-missing");
  assert.equal(state.c, "POST-result");
});

test("release() cancels only the caller's own request and never a newer one", () => {
  const g = createLatestGuard();
  const t1 = g.begin("a");
  g.release("a", t1);
  assert.equal(g.isCurrent("a", t1), false, "the released request can no longer apply its response");
  const effectToken = g.begin("a");
  const generateReadBack = g.begin("a");            // a Generate read-back starts after the effect's request
  g.release("a", effectToken);                       // the effect's cleanup runs late
  assert.equal(g.isCurrent("a", generateReadBack), true, "cleanup of an older request must not cancel the newer read-back");
  g.release("a", generateReadBack);
  assert.equal(g.isCurrent("a", generateReadBack), false);
  g.release("never-seen", 7);                        // releasing an unknown key or token is harmless
  assert.equal(g.isCurrent("never-seen", 7), false);
  const k = g.begin("b"); g.release("a", k);         // another key's token never affects b
  assert.equal(g.isCurrent("b", k), true);
});

test("effect cleanup: a response that arrives after cleanup or unmount is ignored, a newer Generate result survives", () => {
  const g = createLatestGuard();
  const state: Record<string, string> = {};
  const apply = (id: string, token: number, value: string) => { if (g.isCurrent(id, token)) state[id] = value; };
  // effect for row a starts its GET, then the user switches rows / the page unmounts: cleanup releases its token
  const effectToken = g.begin("a");
  g.release("a", effectToken);
  apply("a", effectToken, "LATE-effect-response");
  assert.equal(state.a, undefined, "nothing is written after cleanup");
  // Generate for row a: POST invalidates, stores its result, starts a guarded read-back
  const effect2 = g.begin("a");
  g.invalidate("a"); state.a = "POST-result";
  const readBack = g.begin("a");
  g.release("a", effect2);                           // the older effect's cleanup runs now: must be a no-op for the read-back
  apply("a", effect2, "LATE-effect2-response");
  assert.equal(state.a, "POST-result");
  apply("a", readBack, "PERSISTED-snapshot");
  assert.equal(state.a, "PERSISTED-snapshot", "the Generate read-back still applies after the older effect was cleaned up");
});
