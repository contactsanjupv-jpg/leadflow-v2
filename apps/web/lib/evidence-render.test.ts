import { test } from "node:test";
import assert from "node:assert/strict";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import EvidencePackage, { EvidencePdfLink } from "../components/EvidencePackage";
import { sampleContent } from "./evidence.fixture";
import { CT_2026_0915_GET, ct20260915Post } from "./ct-2026-0915.fixture";
import { selectEvidence } from "./evidence";

const render = (evidence: unknown, source?: "generated" | "stored") => renderToStaticMarkup(createElement(EvidencePackage, { evidence, source }));
const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));
const synthetic = () => ({ id: "p", discrepancy_id: "d-1", content: sampleContent() });
const TITLES = ["Evidence summary", "Invoice details", "Vendor details", "Contract and applicable rates", "Manifest and supporting records", "Reconciliation calculations", "Discrepancy details", "Calculation trace", "Source documents"];

test("the package renders as structured sections in the required order", () => {
  for (const pkg of [synthetic(), CT_2026_0915_GET]) {
    const html = render(pkg);
    let at = -1;
    for (const t of TITLES) { const i = html.indexOf(`<h3>${t}</h3>`); assert.ok(i > at, `${t} missing or out of order`); at = i; }
  }
});

test("CT-2026-0915 renders the real figures and the real absences", () => {
  const html = render(CT_2026_0915_GET, "stored");
  for (const m of ["$4,500.00", "$5,200.00", "$700.00", "$450.00", "$520.00", "RULE_03_RATE_MISMATCH", "CT-2026-0915", "Example Vendor", "MAN-0915"]) assert.ok(html.includes(m), m);
  assert.ok(html.includes("No weight tickets are recorded for this manifest"));
  assert.ok(html.includes("No end date (open-ended)"));
  assert.ok(html.includes("No source document is linked"));
  assert.ok(!html.includes('data-testid="evidence-source-document-link"'), "no download link is offered for a document that does not exist");
  assert.ok(!html.includes("undefined") && !html.includes(">null<"));
});

test("raw JSON is not the default presentation", () => {
  for (const pkg of [synthetic(), CT_2026_0915_GET]) {
    const html = render(pkg);
    assert.ok(!html.includes("<pre") && !html.includes("format_version") && !html.includes("{&quot;"));
    assert.ok(html.includes('data-testid="evidence-raw-toggle"') && html.includes("Show raw JSON"));
  }
});

test("a missing effective_to says Not recorded, and is never shown as open-ended", () => {
  const c = clone(CT_2026_0915_GET) as any; delete c.content.applicable_rates[0].effective_to;
  const html = render(c);
  assert.ok(!html.includes("open-ended"));
  const rates = html.slice(html.indexOf("Contract and applicable rates"), html.indexOf("Manifest and supporting records"));
  assert.ok(rates.includes("Not recorded in this package"));
});

test("missing versus empty: manifest, weight tickets, lists and source documents each say the right thing", () => {
  const c = clone(CT_2026_0915_GET) as any;
  delete c.content.weight_tickets; delete c.content.priced_lines; delete c.content.reconciliation.calculation_trace; delete c.content.invoice.source_document_id;
  let html = render(c);
  for (const m of ["Weight tickets are not recorded in this package", "Priced lines are not recorded in this package", "The calculation trace is not recorded in this package"]) assert.ok(html.includes(m), m);
  c.content.manifest = null; html = render(c);
  assert.ok(html.includes("No manifest is linked to this reconciliation"));
  delete c.content.manifest; html = render(c);
  assert.ok(html.includes("The manifest is not recorded in this package"));
  c.content.invoice.lines = []; c.content.priced_lines = []; c.content.reconciliation.calculation_trace = [];
  html = render(c);
  for (const m of ["No invoice line items are recorded", "No priced lines are listed", "The calculation trace is empty"]) assert.ok(html.includes(m), m);
});

test("linked source documents show their name and a download link", () => {
  const html = render(synthetic());
  assert.ok(html.includes('href="/api/proxy/documents/doc-invoice/download"') && html.includes("CT-2026-0915.pdf") && html.includes("clean-harbors-contract.pdf"));
  assert.ok(html.includes("name not included in this package"));
  assert.ok(html.includes("cap: maximum allowed") && html.includes("unclassified: type not set"));
});

test("a stored snapshot is labelled as stored with its timestamp; a new one is labelled as just generated", () => {
  const stored = render(CT_2026_0915_GET, "stored");
  assert.ok(stored.includes("Stored snapshot generated 2026-"), "uses the stored created_at");
  assert.ok(stored.includes("Regenerate evidence package to create a new snapshot"));
  assert.ok(!/saved/i.test(stored.replace(/<[^>]*>/g, " ")), "the e2e review step asserts getByText(\"Saved\") (a case-insensitive substring match): no evidence text may contain \"saved\"");
  const fresh = render(ct20260915Post(), "generated");
  assert.ok(fresh.includes("Snapshot generated just now"));
  assert.ok(!render(CT_2026_0915_GET).includes('data-testid="evidence-source-note"'), "no label when the source is not given");
});

test("a malformed or unavailable response shows an error, not a crash or a JSON dump", () => {
  for (const bad of [null, "boom", {}, { content: [] }]) {
    const html = render(bad);
    assert.ok(html.includes('data-testid="evidence-error"') && html.includes("Evidence package unavailable"), JSON.stringify(bad));
    assert.ok(!html.includes("<pre"));
  }
});

test("the Download PDF action keeps its link, attribute and test id", () => {
  const html = renderToStaticMarkup(createElement(EvidencePdfLink, { discrepancyId: "d-1" }));
  assert.ok(html.includes('href="/api/proxy/discrepancies/d-1/evidence/pdf"') && html.includes("download") && html.includes('data-testid="evidence-pdf-link"') && html.includes(">Download PDF<"));
});

test("tables sit in horizontally scrollable containers for small screens", () => {
  const html = render(synthetic());
  assert.ok(html.split("<table").length - 1 > 0 && html.split('class="evscroll"').length >= html.split("<table").length);
});

test("the review page shows stored snapshots read-only (GET), never regenerates on open, and keeps every action", async () => {
  const { readFileSync } = await import("node:fs");
  const src = readFileSync(new URL("../app/(app)/review/page.tsx", import.meta.url), "utf8");
  assert.ok(src.includes("selectEvidence(evidence, stored, d.id)") && src.includes("<EvidencePackage evidence={shown.evidence} source={shown.source} />"));
  assert.ok(src.includes("<EvidencePdfLink discrepancyId={d.id} />"));
  assert.ok(!src.includes("JSON.stringify") && !src.includes("<pre"), "the page must not render raw JSON itself");
  const effect = src.slice(src.indexOf("useEffect"), src.indexOf("async function review"));
  assert.ok(effect.includes('api("GET"') && !effect.includes('"POST"'), "opening a row only reads the stored snapshot");
  assert.equal(src.split('api("POST", `/discrepancies/${id}/evidence`').length - 1, 1, "the only generating call is the Generate button's");
  for (const label of ["Start review", "Confirm", "Reject", "Needs information", "Reopen", "Open dispute"]) assert.ok(src.includes(label), `${label} action kept`);
});

test("after Generate, once the persisted snapshot is read back, the page shows it with its real created_at", () => {
  const id = (CT_2026_0915_GET as { discrepancy_id: string }).discrepancy_id;
  const before = selectEvidence(ct20260915Post(), { [id]: ct20260915Post() }, id)!;
  assert.equal(before.source, "generated");
  assert.ok(render(before.evidence, before.source).includes("Snapshot generated just now"));
  const after = selectEvidence(ct20260915Post(), { [id]: CT_2026_0915_GET }, id)!;   // generated response still held in state
  assert.equal(after.source, "stored");
  const html = render(after.evidence, after.source);
  assert.ok(html.includes("Stored snapshot generated 2026-") && !html.includes("generated just now"));
});

test("the review page wires the guard: reads and the post-generate read-back are guarded, and POST supersedes older reads", async () => {
  const { readFileSync } = await import("node:fs");
  const src = readFileSync(new URL("../app/(app)/review/page.tsx", import.meta.url), "utf8");
  const effect = src.slice(src.indexOf("useEffect"), src.indexOf("async function review"));
  assert.ok(effect.includes("latest.current.begin(id)") && effect.includes("latest.current.isCurrent(id, token)"));
  assert.equal(effect.split("isCurrent(id, token)").length - 1, 2, "both the success and the failure path are guarded");
  const make = src.slice(src.indexOf("async function makeEvidence"), src.indexOf("async function dispute"));
  const inval = make.indexOf("latest.current.invalidate(id)"), post = make.indexOf("setStored((s) => ({ ...s, [id]: r }))"), begin = make.indexOf("latest.current.begin(id)");
  assert.ok(inval >= 0 && inval < post && post < begin, "invalidate, then store the POST result, then start the guarded read-back");
  assert.ok(make.includes("isCurrent(id, token)") && make.includes('api("GET"'), "the read-back only applies while current");
  assert.ok(src.includes("useRef") && src.includes("createLatestGuard"));
});

test("the review page's stored-evidence effect returns a cleanup that releases only its own token", async () => {
  const { readFileSync } = await import("node:fs");
  const src = readFileSync(new URL("../app/(app)/review/page.tsx", import.meta.url), "utf8");
  const effect = src.slice(src.indexOf("useEffect"), src.indexOf("async function review"));
  assert.ok(effect.includes("return () => latest.current.release(id, token);"), "cleanup releases the effect's own request");
  assert.ok(effect.indexOf("latest.current.begin(id)") < effect.indexOf("return () =>"), "cleanup is registered after the request starts");
  assert.ok(!effect.includes("invalidate("), "the effect must not invalidate the row (that would cancel a newer Generate read-back)");
  const make = src.slice(src.indexOf("async function makeEvidence"), src.indexOf("async function dispute"));
  assert.ok(make.includes("latest.current.invalidate(id)"), "only the Generate path supersedes in-flight reads");
});
