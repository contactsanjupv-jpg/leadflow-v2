// Step 14: strict identifiers and metadata in the evidence view model.
//  - source-document ids must be canonical UUIDs (the backend download route takes a Uuid): anything else never becomes a link
//  - a recorded sha256 must be exactly 64 lowercase hex characters (what the backend writes)
//  - a manifest unit must be a non-empty string; a present-but-malformed unit makes the Quantity unreadable
// Missing keys and explicit nulls keep the meaning they had before.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import EvidencePackage from "../components/EvidencePackage";
import { buildEvidenceView, countUnreadable, hasEvidenceIssues, type EvidenceView, type Field } from "./evidence";
import { sampleContent } from "./evidence.fixture";

const U_INV = "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a61";
const U_CON = "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a62";
const U_MAN = "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a63";
const U_TKT = "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a64";
const U_SUP = "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a65";

const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));
const view = (raw: unknown): EvidenceView => { const r = buildEvidenceView(raw); assert.equal(r.ok, true); return (r as Extract<typeof r, { ok: true }>).view; };
const html = (raw: unknown) => renderToStaticMarkup(createElement(EvidencePackage, { evidence: raw }));
const text = (h: string) => h.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ");
const f = (fs: Field[], label: string) => { const x = fs.find((y) => y.label === label); assert.ok(x, label); return x!; };

/** A package whose four document references and the document list are all well-formed UUIDs. */
function pkg(): any {
  const c: any = clone(sampleContent());
  c.invoice.source_document_id = U_INV;
  c.manifest.source_document_id = U_MAN;
  c.applicable_rates[0].contract_source_document_id = U_CON;
  c.contract_rate.contract_source_document_id = U_CON;
  c.weight_tickets[0].source_document_id = U_TKT;
  c.source_documents = [
    { id: U_INV, document_type: "INVOICE", original_filename: "invoice.pdf", sha256: "a".repeat(64), created_at: "t" },
    { id: U_CON, document_type: "CONTRACT", original_filename: "contract.pdf", sha256: "b".repeat(64), created_at: "t" },
  ];
  return c;
}
const HREF_ANY = "/api/proxy/documents/";
const LINK_TESTID = 'data-testid="evidence-source-document-link"';

// A reference is set at one of the four places a record points at a document.
const POSITIONS: { name: string; role: (r: string) => boolean; set: (c: any, v: unknown) => void; del: (c: any) => void }[] = [
  { name: "invoice", role: (r) => r === "Invoice", set: (c, v) => { c.invoice.source_document_id = v; }, del: (c) => { delete c.invoice.source_document_id; } },
  { name: "manifest", role: (r) => r === "Manifest", set: (c, v) => { c.manifest.source_document_id = v; }, del: (c) => { delete c.manifest.source_document_id; } },
  { name: "contract", role: (r) => r.startsWith("Contract"), set: (c, v) => { c.applicable_rates[0].contract_source_document_id = v; }, del: (c) => { delete c.applicable_rates[0].contract_source_document_id; } },
  { name: "weight ticket", role: (r) => r.startsWith("Weight ticket"), set: (c, v) => { c.weight_tickets[0].source_document_id = v; }, del: (c) => { delete c.weight_tickets[0].source_document_id; } },
];

const BAD_IDS: unknown[] = [
  "..", ".", "../" + U_INV, U_INV + "/..", U_INV + "/x", "%2e%2e", "doc-1", "not-a-uuid", "42",
  U_INV + "x", "x" + U_INV, " " + U_INV, U_INV + " ", U_INV + "\n", "{" + U_INV + "}", U_INV.replace(/-/g, ""),
  U_INV.slice(0, -1), U_INV + "0", U_INV.replace(/^5b1f6c2e/, "5b1f6c2g"), "5b1f6c2e-8a3d-4f60-9b7a-1c2d3e4f5a6/",
  42, 0, true, false, { id: U_INV }, [U_INV], "", "   ",
];

// ---- 1. source-document references ----------------------------------------------------------------------
test("UUID references: canonical UUIDs in any letter case become download links", () => {
  for (const p of POSITIONS) {
    for (const id of [U_INV, U_INV.toUpperCase(), "5B1f6c2E-8a3D-4F60-9b7A-1c2d3e4F5a61"]) {
      const c = pkg(); p.set(c, id);
      const l = view(c).documents.find((d) => p.role(d.role) && d.doc?.id === id);
      assert.ok(l, `${p.name} ${id}`);
      assert.equal(l!.state, "linked");
      assert.equal(l!.doc!.href, `${HREF_ANY}${id}/download`);
    }
  }
});

test("UUID references: every malformed id at every reference position is unreadable and never produces a link", () => {
  for (const p of POSITIONS) {
    for (const bad of BAD_IDS) {
      const c = pkg(); p.set(c, bad);
      // keep the document list empty of the bad id so only the reference is under test
      const label = `${p.name}=${JSON.stringify(bad)}`;
      const v = view(c);
      const l = v.documents.find((d) => p.role(d.role) && d.state !== "linked");
      assert.ok(l, `${label}: the reference is reported`);
      assert.equal(l!.state, "unreadable", label);
      assert.equal(l!.doc, null, label);
      assert.ok(!v.documents.some((d) => d.doc && p.role(d.role) && d.doc.id === String(bad)), label);
      for (const d of v.documents) if (d.doc) assert.match(d.doc.href, /^\/api\/proxy\/documents\/[0-9a-fA-F-]{36}\/download$/, label);
      assert.ok(hasEvidenceIssues(v) && countUnreadable(v) >= 1, label);
    }
  }
});

test("UUID references: a malformed id is never rendered as a download link or an href", () => {
  for (const p of POSITIONS) {
    for (const bad of [".", "..", "not-a-uuid", 42, U_INV + "/x"]) {
      const c = pkg(); c.source_documents = []; p.set(c, bad);
      // with no other document linked at this position, the only links left come from the other three references
      const h = html(c);
      assert.ok(!h.includes(`${HREF_ANY}${String(bad)}`), `${p.name}=${String(bad)}`);
      assert.ok(!h.includes(`${HREF_ANY}../`) && !h.includes(`${HREF_ANY}./`), `${p.name}=${String(bad)}`);
      assert.ok(!h.includes(`/api/proxy/documents/${encodeURIComponent(String(bad))}/download`), `${p.name}=${String(bad)}`);
    }
  }
  // a package whose ONLY references are malformed has no download link at all
  const c = pkg(); c.source_documents = [];
  for (const p of POSITIONS) p.set(c, "..");
  const h = html(c);
  assert.ok(!h.includes(LINK_TESTID) && !h.includes(HREF_ANY), "no link, no href");
  assert.ok(text(h).includes("Unreadable source-document reference"), "the malformed references are flagged on screen");
});

test("UUID references: missing keys stay 'absent' and explicit null stays 'none' (unchanged)", () => {
  for (const p of POSITIONS) {
    const a = pkg(); p.del(a);
    const n = pkg(); p.set(n, null);
    const la = view(a).documents.find((d) => p.role(d.role) && d.state !== "linked");
    const ln = view(n).documents.find((d) => p.role(d.role) && d.state !== "linked");
    assert.equal(la?.state, "absent", `${p.name} missing`);
    assert.equal(ln?.state, "none", `${p.name} null`);
    assert.equal(ln?.doc ?? null, null);
  }
  const n = pkg(); POSITIONS[0].set(n, null);
  assert.ok(!hasEvidenceIssues(view(n)), "an explicit null reference raises no warning");
});

// ---- 2. entries in source_documents --------------------------------------------------------------------
test("source_documents: entries with a malformed id are flagged as an incomplete list and never listed or linked", () => {
  const c = pkg();
  c.source_documents = [
    { id: 42, document_type: "OTHER", original_filename: "numeric.pdf", sha256: "c".repeat(64) },
    { id: "doc-1", document_type: "OTHER", original_filename: "named.pdf", sha256: "c".repeat(64) },
    { id: "..", document_type: "OTHER", original_filename: "dots.pdf", sha256: "c".repeat(64) },
    { id: U_INV + "/x", document_type: "OTHER", original_filename: "suffix.pdf", sha256: "c".repeat(64) },
    { id: U_SUP, document_type: "OTHER", original_filename: "good.pdf", sha256: "c".repeat(64) },
  ];
  const v = view(c);
  assert.deepEqual(v.issues.sourceDocuments, { kind: "partial", skipped: 4, total: 5 });
  const ids = v.documents.flatMap((d) => (d.doc ? [d.doc.id] : []));
  assert.ok(ids.includes(U_SUP), "the well-formed document is still listed");
  for (const bad of ["42", "doc-1", "..", U_INV + "/x"]) assert.ok(!ids.includes(bad), bad);
  const h = html(c);
  assert.ok(h.includes('data-testid="evidence-incomplete-banner"'));
  assert.ok(!h.includes("numeric.pdf") && !h.includes("named.pdf") && !h.includes("dots.pdf") && !h.includes("suffix.pdf"));
  assert.ok(!h.includes(`${HREF_ANY}42/`) && !h.includes(`${HREF_ANY}doc-1/`) && !h.includes(`${HREF_ANY}../`));
});

test("source_documents: when every entry is malformed there is no document link and the list is flagged", () => {
  const c = pkg(); c.source_documents = [{ id: 1 }, { id: "x" }, { id: ".." }];
  for (const p of POSITIONS) p.set(c, null);
  const v = view(c);
  assert.deepEqual(v.issues.sourceDocuments, { kind: "partial", skipped: 3, total: 3 });
  assert.equal(v.documents.some((d) => d.doc !== null), false);
  assert.ok(!html(c).includes(LINK_TESTID));
});

test("source_documents: an empty list, a missing list and an explicit null list are not flagged (unchanged)", () => {
  for (const mut of [(c: any) => { c.source_documents = []; }, (c: any) => { delete c.source_documents; }, (c: any) => { c.source_documents = null; }]) {
    const c = pkg(); mut(c);
    assert.equal(view(c).issues.sourceDocuments, null);
  }
});

test("source_documents: a reference and a listed document match whatever the letter case, and are not shown twice", () => {
  const c = pkg();
  c.invoice.source_document_id = U_INV.toUpperCase();
  const v = view(c);
  const inv = v.documents.find((d) => d.role === "Invoice")!;
  assert.equal(inv.state, "linked");
  assert.equal(inv.doc!.filename, "invoice.pdf", "metadata of the listed document is found");
  assert.ok(!v.documents.some((d) => d.role === "Supporting document" && d.doc?.id.toLowerCase() === U_INV), "not repeated as a supporting document");
});

test("source_documents: a well-formed listed document that nothing points at is still shown as a supporting document", () => {
  const c = pkg(); c.source_documents.push({ id: U_SUP, document_type: "OTHER", original_filename: "extra.pdf", sha256: "c".repeat(64) });
  const s = view(c).documents.find((d) => d.role === "Supporting document" && d.doc?.id === U_SUP);
  assert.ok(s && s.doc!.href === `${HREF_ANY}${U_SUP}/download`);
});

// ---- 3. sha256 ----------------------------------------------------------------------------------------
const withSha = (sha: unknown) => { const c = pkg(); c.source_documents[0].sha256 = sha; return c; };
const docOf = (c: unknown) => view(c).documents.find((d) => d.role === "Invoice")!.doc!;
const HEX64 = "0123456789abcdef".repeat(4);

test("sha256: exactly 64 lowercase hex characters is accepted and raises no warning", () => {
  for (const ok of [HEX64, "a".repeat(64), "0".repeat(64), "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"]) {
    const d = docOf(withSha(ok));
    assert.equal(d.sha256, ok);
    assert.equal(d.detailsUnreadable, false);
  }
});

test("sha256: uppercase, padding, wrong length, non-hex characters and prefixes are unreadable and never shown", () => {
  const bad: unknown[] = [
    HEX64.toUpperCase(), "A" + "a".repeat(63), " " + HEX64, HEX64 + " ", HEX64 + "\n", HEX64.slice(0, 63), HEX64 + "0", "abc", "zzzz",
    "g" + "a".repeat(63), "sha256:" + HEX64, "0x" + HEX64.slice(0, 62), HEX64.slice(0, 32) + " " + HEX64.slice(33), "é".repeat(64),
    "", "   ", 424242, 0, true, false, { a: 1 }, [HEX64],
  ];
  for (const b of bad) {
    const c = withSha(b);
    const d = docOf(c);
    assert.equal(d.sha256, null, `sha256=${JSON.stringify(b)} must not be displayed`);
    assert.equal(d.detailsUnreadable, true, `sha256=${JSON.stringify(b)}`);
    assert.ok(hasEvidenceIssues(view(c)) && countUnreadable(view(c)) >= 1);
    const h = html(c);
    assert.ok(text(h).includes("Some document details could not be read") && h.includes('data-testid="evidence-incomplete-banner"'), JSON.stringify(b));
    if (typeof b === "string" && b.trim().length >= 12) assert.ok(!h.includes(b.slice(0, 12) + "…"), "the bad value is not displayed as a hash");
  }
});

test("sha256: missing and explicit null stay legitimate (no value, not flagged)", () => {
  const missing = pkg(); delete missing.source_documents[0].sha256;
  for (const c of [missing, withSha(null)]) {
    const d = docOf(c);
    assert.equal(d.sha256, null);
    assert.equal(d.detailsUnreadable, false);
    assert.ok(!html(c).includes("Some document details could not be read"));
  }
});

test("sha256: a bad hash does not hide the readable filename and type; and a bad filename does not hide a good hash", () => {
  const d = docOf(withSha("not-a-hash"));
  assert.equal(d.filename, "invoice.pdf");
  assert.equal(d.documentType, "INVOICE");
  const c = pkg(); c.source_documents[0].original_filename = 123;
  const e = docOf(c);
  assert.equal(e.filename, null);
  assert.equal(e.sha256, "a".repeat(64));
  assert.equal(e.detailsUnreadable, true);
});

test("original_filename and document_type behave as before: any non-empty string is readable", () => {
  const c = pkg(); c.source_documents[0].original_filename = "Weird Name (1).PDF"; c.source_documents[0].document_type = "whatever";
  const d = docOf(c);
  assert.deepEqual([d.filename, d.documentType, d.detailsUnreadable], ["Weird Name (1).PDF", "whatever", false]);
});

// ---- 4. manifest unit ---------------------------------------------------------------------------------
const manifestFields = (c: unknown) => { const m = view(c).manifest; assert.equal(m.state, "present"); return (m as Extract<typeof m, { state: "present" }>).fields; };
const withUnit = (unit: unknown) => { const c = pkg(); c.manifest.unit = unit; return c; };

test("manifest unit: a non-empty string unit is shown after the quantity", () => {
  const q = f(manifestFields(withUnit("ton")), "Quantity");
  assert.equal(q.state, "value");
  assert.equal(q.value, "10.0000 ton");
  assert.equal(f(manifestFields(withUnit("cubic yard")), "Quantity").value, "10.0000 cubic yard");
});

test("manifest unit: a present number, object, array, boolean, blank string or explicit null makes the Quantity unreadable", () => {
  for (const bad of [5, 0, 1.5, true, false, { u: "ton" }, {}, ["ton"], [], "", "   ", null]) {
    const c = withUnit(bad);
    const q = f(manifestFields(c), "Quantity");
    assert.equal(q.state, "unreadable", `unit=${JSON.stringify(bad)}`);
    assert.equal(q.value, null, `unit=${JSON.stringify(bad)}: no partial quantity`);
    const v = view(c);
    assert.ok(hasEvidenceIssues(v) && countUnreadable(v) >= 1, JSON.stringify(bad));
    const h = html(c);
    assert.ok(h.includes('data-testid="evidence-incomplete-banner"'), JSON.stringify(bad));
    assert.ok(!text(h).includes("10.0000 5") && !text(h).includes("10.0000 true") && !text(h).includes("10.0000 [object"), JSON.stringify(bad));
  }
});

test("manifest unit: a numeric unit is not converted to text", () => {
  const h = text(html(withUnit(5)));
  assert.ok(!h.includes("10.0000 5"));
});

test("manifest unit: a missing unit key shows the quantity without a unit (unchanged)", () => {
  const c = pkg(); delete c.manifest.unit;
  const q = f(manifestFields(c), "Quantity");
  assert.equal(q.state, "value");
  assert.equal(q.value, "10.0000");
  assert.ok(!hasEvidenceIssues(view(c)));
});

test("manifest unit: a malformed unit makes the Quantity unreadable even when the quantity is itself missing, null or unreadable", () => {
  for (const mut of [(m: any) => { delete m.quantity; }, (m: any) => { m.quantity = null; }, (m: any) => { m.quantity = { x: 1 }; }]) {
    const c = withUnit(5); mut(c.manifest);
    const q = f(manifestFields(c), "Quantity");
    assert.equal(q.state, "unreadable");
    assert.equal(q.value, null);
  }
});

test("manifest unit: a valid unit does not change how a missing, null or unreadable quantity is reported (unchanged)", () => {
  const a = pkg(); delete a.manifest.quantity;
  const n = pkg(); n.manifest.quantity = null;
  const u = pkg(); u.manifest.quantity = 10;
  assert.equal(f(manifestFields(a), "Quantity").state, "absent");
  assert.equal(f(manifestFields(n), "Quantity").state, "null");
  assert.equal(f(manifestFields(u), "Quantity").state, "unreadable");
});

test("manifest unit: other manifest fields are unaffected by a malformed unit", () => {
  const fs = manifestFields(withUnit({ u: 1 }));
  assert.equal(f(fs, "Manifest number").value, "MAN-77");
  assert.equal(f(fs, "Waste stream").value, "F003");
  assert.equal(f(fs, "Containers").value, "4");
});
