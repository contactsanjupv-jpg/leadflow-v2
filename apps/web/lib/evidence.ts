// View model for the evidence package shown on the review queue. It only re-shapes what the API
// returned: nothing is computed, defaulted or inferred. Money stays an exact decimal STRING and is
// formatted with formatMoney (no floating point).
//
// Every value keeps FOUR distinct states, because they mean different things:
//   "value"      the package records a value
//   "null"       the package explicitly records "no value" (e.g. effective_to = null: the rate has no end date)
//   "absent"     the key is not in the package at all (older snapshots, or a field this package never carried)
//   "unreadable" the key IS in the package but its value is malformed (wrong type, blank, a non-decimal amount)
// "absent" is shown as "Not recorded in this package" and "unreadable" as a warning. Neither is turned into a fact,
// and a malformed record is never displayed as though it were simply missing.
import { formatMoney } from "./money";

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
// Identifiers and hashes are validated, not just stringified. The backend download route takes a Uuid, and it
// writes the document hash as 64 lowercase hex characters (documents.rs), so anything else is "unreadable".
const UUID = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const SHA256 = /^[0-9a-f]{64}$/;
const uuidText = (v: unknown): string | null => (typeof v === "string" && UUID.test(v) ? v : null);
const sha256Text = (v: unknown): string | null => (typeof v === "string" && SHA256.test(v) ? v : null);
const text = (v: unknown): string | null => (typeof v === "string" && v.trim() !== "" ? v : typeof v === "number" ? String(v) : null);
const has = (o: Obj | null, k: string): o is Obj => o !== null && Object.prototype.hasOwnProperty.call(o, k);

export type FieldState = "value" | "null" | "absent" | "unreadable";
/** An object the package carries: readable, missing/null, or present but not an object at all. */
type Src = Obj | null | "unreadable";
const obj = (s: Src): Obj | null => (s === "unreadable" ? null : s);
function srcOf(o: Obj, key: string): Src { if (!has(o, key) || o[key] === null) return null; return isObj(o[key]) ? (o[key] as Obj) : "unreadable"; }
export interface Cell { state: FieldState; value: string | null; money?: boolean; nullText?: string }
export interface Field extends Cell { label: string }

interface CellOpts { money?: boolean; nullText?: string; format?: (v: unknown) => string | null; allowNumber?: boolean }

// Amounts are exact decimal STRINGS (project rule: never floats), so a number or non-decimal text is malformed.
const DECIMAL = /^-?\d+(\.\d+)?$/;
const readText = (raw: unknown, allowNumber: boolean): string | null =>
  typeof raw === "string" ? (raw.trim() !== "" ? raw : null) : allowNumber && typeof raw === "number" && Number.isFinite(raw) ? String(raw) : null;

/** Read one key as a Cell, keeping recorded-null, missing and malformed apart. */
export function cell(o: Src, key: string, opts: CellOpts = {}): Cell {
  const base = { money: opts.money, nullText: opts.nullText };
  if (o === "unreadable") return { ...base, state: "unreadable", value: null }; // the object holding this key is malformed
  if (!has(o, key)) return { ...base, state: "absent", value: null };
  const raw = o[key];
  if (raw === null) return { ...base, state: "null", value: null };
  const s = opts.format ? opts.format(raw)
    : opts.money ? (typeof raw === "string" && DECIMAL.test(raw) ? formatMoney(raw) : null)
    : readText(raw, !!opts.allowNumber);
  // Present but malformed (wrong type, blank, a non-decimal amount): flagged, never shown as "not recorded".
  return s === null ? { ...base, state: "unreadable", value: null } : { ...base, state: "value", value: s };
}
const field = (label: string, o: Src, key: string, opts: CellOpts = {}): Field => ({ label, ...cell(o, key, opts) });

/**
 * What was wrong with a list in the package. A list the reader could not fully use is NEVER presented as
 * complete: "unreadable" means the key holds something that is not a list at all; "partial" means some
 * entries were not records (or, for source documents, had no usable id) and are not shown.
 */
export type ListIssue = { kind: "unreadable" } | { kind: "partial"; skipped: number; total: number };

interface Listed { rows: Obj[] | null; issue: ListIssue | null }

/**
 * Four distinct states, kept apart:
 *   missing key or recorded null -> rows null, no issue   (not recorded)
 *   []                           -> rows [], no issue     (recorded as empty)
 *   valid records                -> rows, no issue
 *   anything malformed           -> an issue (the readable rows are still returned, nothing is hidden silently)
 */
function listOf(o: Src, key: string, usable: (r: Obj) => boolean = () => true): Listed {
  if (o === "unreadable") return { rows: null, issue: { kind: "unreadable" } }; // the record holding the list is malformed
  if (!has(o, key) || o[key] === null) return { rows: null, issue: null };
  const v = o[key];
  if (!Array.isArray(v)) return { rows: null, issue: { kind: "unreadable" } };
  const objs = v.filter(isObj);
  const rows = objs.filter(usable);
  const skipped = v.length - rows.length;
  return { rows, issue: skipped > 0 ? { kind: "partial", skipped, total: v.length } : null };
}

export interface DocRef { id: string; filename: string | null; documentType: string | null; sha256: string | null; href: string; detailsUnreadable: boolean }
export interface DocLink { role: string; state: "linked" | "none" | "absent" | "unreadable"; doc: DocRef | null }

export interface RateView {
  id: string | null;
  fields: Field[];
  pricedLines: Cell;
  fees: { code: Cell; description: Cell; amount: Cell; typeLabel: string }[] | null;
  feesIssue: ListIssue | null;
}

export type ManifestView =
  | { state: "absent" }
  | { state: "none" }
  | { state: "unreadable" }
  | { state: "present"; fields: Field[]; tickets: { number: Cell; netWeight: Cell; unit: Cell; date: Cell }[] | null };

export interface EvidenceView {
  summary: Field[];
  invoice: { fields: Field[]; lines: { description: Cell; feeCode: Cell; quantity: Cell; unit: Cell; unitPrice: Cell; lineTotal: Cell }[] | null };
  vendor: Field[];
  /** parentUnconfirmed: the applicable-rate list is flagged malformed and yielded no readable rate, so the parent contract rate is
   *  all that is left. It is recorded evidence but must not be presented as the rate that applied. */
  rates: { basisLabel: string | null; basisUnreadable: boolean; applicable: RateView[] | null; parent: RateView | null; parentUnreadable: boolean; parentUnconfirmed: boolean };
  manifest: ManifestView;
  calculations: { fields: Field[]; pricedLines: { description: Cell; quantity: Cell; unit: Cell; unitRate: Cell; expected: Cell; contract: Cell }[] | null };
  discrepancy: Field[];
  trace: string[] | null;
  documents: DocLink[];
  /** Lists that were malformed, by list. All null for a package whose lists are all readable. */
  issues: Record<"invoiceLines" | "authorizedFees" | "weightTickets" | "applicableRates" | "pricedLines" | "sourceDocuments" | "trace", ListIssue | null>;
  /** Objects the package carries that are present but not objects at all. */
  objects: Record<"discrepancy" | "reconciliation" | "invoice" | "contractRate" | "manifest", boolean>;
}

/** True when any list in the package could not be fully read. */
export function hasListIssues(v: EvidenceView): boolean {
  return Object.values(v.issues).some((i) => i !== null) || (v.rates.applicable ?? []).concat(v.rates.parent ? [v.rates.parent] : []).some((r) => r.feesIssue !== null);
}

/** Count of malformed scalar values / document references anywhere in the view. */
export function countUnreadable(v: EvidenceView): number {
  let n = 0;
  const c = (x: Cell) => { if (x.state === "unreadable") n++; };
  v.summary.forEach(c); v.vendor.forEach(c); v.discrepancy.forEach(c); v.invoice.fields.forEach(c);
  v.invoice.lines?.forEach((l) => [l.description, l.feeCode, l.quantity, l.unit, l.unitPrice, l.lineTotal].forEach(c));
  for (const r of [...(v.rates.applicable ?? []), ...(v.rates.parent ? [v.rates.parent] : [])]) {
    r.fields.forEach(c); c(r.pricedLines); r.fees?.forEach((f) => [f.code, f.description, f.amount].forEach(c));
  }
  if (v.manifest.state === "present") { v.manifest.fields.forEach(c); v.manifest.tickets?.forEach((t) => [t.number, t.netWeight, t.unit, t.date].forEach(c)); }
  v.calculations.fields.forEach(c);
  v.calculations.pricedLines?.forEach((p) => [p.description, p.quantity, p.unit, p.unitRate, p.expected, p.contract].forEach(c));
  for (const d of v.documents) if (d.state === "unreadable" || d.doc?.detailsUnreadable) n++;
  return n;
}

/** True when ANYTHING in the package was malformed: a list, an object, a scalar value or a document reference. */
export function hasEvidenceIssues(v: EvidenceView): boolean {
  return hasListIssues(v) || countUnreadable(v) > 0 || Object.values(v.objects).some(Boolean) || v.rates.parentUnreadable || v.rates.basisUnreadable;
}

export type EvidenceResult = { ok: true; view: EvidenceView } | { ok: false; reason: string };

/** Same wording as the PDF's fee_charge_label: a missing type is never turned into EXACT or CAP. */
export function feeChargeLabel(fee: Obj): string {
  if (!("charge_type" in fee)) return "type not recorded in this evidence package";
  const t = fee.charge_type;
  if (t === null) return "unclassified: type not set";
  if (t === "EXACT") return "exact charge";
  if (t === "CAP") return "cap: maximum allowed";
  return "unrecognised charge type";
}

const BASIS: Record<string, string> = {
  recorded: "Recorded when this invoice was reconciled",
  line_rates_only: "Reconstructed from the rates that priced invoice lines (the full applicable set was not recorded)",
  none: "No applicable rate was recorded for this reconciliation",
};

// contract_rates.effective_to is a nullable DATE (migration 0001: CHECK effective_to IS NULL OR
// effective_to >= effective_from) and a NULL end date is how an open-ended rate is stored, so an
// explicitly recorded null means "no end date". A MISSING key is not recorded and is never open-ended.
const OPEN_ENDED = "No end date (open-ended)";

function rateView(r: Obj): RateView {
  const feeList = listOf(r, "authorized_fees");
  const fees = feeList.rows;
  return {
    id: text(r.contract_rate_id) ?? text(r.id),
    fields: [
      field("Contract", r, "contract_name"),
      field("Waste stream", r, "waste_stream"),
      field("Unit", r, "unit"),
      field("Rate", r, "unit_rate", { money: true }),
      field("Effective from", r, "effective_from"),
      field("Effective to", r, "effective_to", { nullText: OPEN_ENDED }),
    ],
    pricedLines: !has(r, "priced_line_ids") ? { state: "absent", value: null }
      : r.priced_line_ids === null ? { state: "null", value: null }
      : Array.isArray(r.priced_line_ids) ? { state: "value", value: String(r.priced_line_ids.length) }
      : { state: "unreadable", value: null },
    feesIssue: feeList.issue,
    fees: fees && fees.map((f) => ({
      code: cell(f, "fee_code"),
      description: cell(f, "description", { nullText: "No description" }),
      amount: cell(f, "amount", { money: true }),
      typeLabel: feeChargeLabel(f),
    })),
  };
}

export function buildEvidenceView(raw: unknown): EvidenceResult {
  // Accept the POST/GET response ({ content }) or the bare content object.
  const content = isObj(raw) && isObj(raw.content) ? raw.content : raw;
  if (!isObj(content)) return { ok: false, reason: "The evidence package is missing or is not in a readable format." };
  const known = ["discrepancy", "reconciliation", "invoice", "contract_rate", "applicable_rates", "priced_lines", "manifest", "source_documents"];
  if (!known.some((k) => k in content)) return { ok: false, reason: "The evidence package has none of the expected sections." };

  // Each nested object is readable, missing/null, or present-but-malformed ("unreadable"). A malformed one is
  // flagged and its fields read as unreadable: it is never treated as though it were simply not recorded.
  const d = srcOf(content, "discrepancy");
  const rec = srcOf(content, "reconciliation");
  const inv = srcOf(content, "invoice");
  const parentSrc = srcOf(content, "contract_rate");
  const parent = obj(parentSrc);

  // Why a withheld amount is null depends on the pricing status the package itself records.
  const recObj = obj(rec);
  const status = recObj ? text(recObj.pricing_status) : null;
  const withheld = status === "PARTIAL" ? "Not computed: the invoice is only partially priced"
    : status === "LEGACY_UNVERIFIED" ? "Not verified: legacy reconciliation" : undefined;

  let trace: string[] | null = null;
  let traceIssue: ListIssue | null = null;
  if (rec === "unreadable") traceIssue = { kind: "unreadable" }; // the reconciliation record holding the trace is malformed
  else if (has(rec, "calculation_trace") && rec.calculation_trace !== null) {
    const t = rec.calculation_trace;
    if (!Array.isArray(t)) traceIssue = { kind: "unreadable" };
    else {
      trace = t.filter((x): x is string => typeof x === "string");
      if (trace.length < t.length) traceIssue = { kind: "partial", skipped: t.length - trace.length, total: t.length };
    }
  }

  const ticketList = listOf(content, "weight_tickets");
  // ---- manifest
  let manifest: ManifestView;
  if (!has(content, "manifest")) manifest = { state: "absent" };
  else if (content.manifest === null) manifest = { state: "none" };
  else if (!isObj(content.manifest)) manifest = { state: "unreadable" };
  else {
    const m = content.manifest;
    const qty = cell(m, "quantity");
    // The unit is a non-empty string. Missing: the quantity is shown without a unit. Present but not a usable string
    // (number, object, array, boolean, blank, explicit null): the quantity is unreadable, never shown half-formed.
    const unit = cell(m, "unit");
    const unitBad = unit.state === "unreadable" || unit.state === "null";
    const tickets = ticketList.rows;
    manifest = {
      state: "present",
      fields: [
        field("Manifest number", m, "manifest_number"),
        field("Waste stream", m, "waste_stream"),
        field("Shipment date", m, "shipment_date"),
        { label: "Quantity", ...(unitBad ? { ...qty, state: "unreadable" as const, value: null } : qty.state === "value" && unit.state === "value" ? { ...qty, value: `${qty.value} ${unit.value}` } : qty) },
        field("Containers", m, "container_count", { allowNumber: true }),
      ],
      tickets: tickets && tickets.map((t) => ({ number: cell(t, "ticket_number"), netWeight: cell(t, "net_weight"), unit: cell(t, "unit"), date: cell(t, "ticket_date") })),
    };
  }

  // ---- source documents: which record points at which document, and whether it points at one at all
  const docList = listOf(content, "source_documents", (r) => uuidText(r.id) !== null);
  const listed = docList.rows ?? [];
  const byId = new Map(listed.map((x) => [(uuidText(x.id) as string).toLowerCase(), x]));
  const docRef = (id: string): DocRef => {
    const k = byId.get(id.toLowerCase());
    // Document metadata is text: only a non-empty STRING is readable (unlike text(), a number is not converted).
    // Missing and explicit null stay as they were (no value, not flagged); anything else present is unreadable.
    const metaText = (v: unknown): string | null => (typeof v === "string" && v.trim() !== "" ? v : null);
    const readers: Record<string, (v: unknown) => string | null> = { original_filename: metaText, document_type: metaText, sha256: sha256Text };
    const detailsUnreadable = !!k && Object.entries(readers).some(([key, read]) => has(k, key) && k[key] !== null && read(k[key]) === null);
    return { id, filename: k ? metaText(k.original_filename) : null, documentType: k ? metaText(k.document_type) : null, sha256: k ? sha256Text(k.sha256) : null, href: `/api/proxy/documents/${encodeURIComponent(id)}/download`, detailsUnreadable };
  };
  const links: DocLink[] = [];
  const link = (role: string, o: Src, key: string) => {
    const c = cell(o, key, { format: uuidText }); // a reference that is not a UUID can never be a download link
    const l: DocLink = { role, state: c.state === "value" ? "linked" : c.state === "null" ? "none" : c.state === "unreadable" ? "unreadable" : "absent", doc: c.state === "value" ? docRef(c.value as string) : null };
    if (!links.some((x) => x.role === l.role && x.state === l.state && x.doc?.id === l.doc?.id)) links.push(l);
  };
  link("Invoice", inv, "source_document_id");
  const applicableList = listOf(content, "applicable_rates");
  // The parent stands in whenever the applicable-rate list is flagged (unreadable, or every entry malformed) and
  // yields no readable rate. One predicate drives both the rates section and the contract document label.
  const parentStandsIn = parent !== null && applicableList.issue !== null && (applicableList.rows ?? []).length === 0;
  const rateObjs = parentStandsIn ? [parent as Obj] : (applicableList.rows ?? (parent ? [parent] : []));
  for (const r of rateObjs.length ? rateObjs : []) link(`Contract${text(r.contract_name) ? " — " + text(r.contract_name) : ""}${parentStandsIn ? " (parent rate, not confirmed as applicable)" : ""}`, r, "contract_source_document_id");
  if (manifest.state === "present") link("Manifest", content.manifest as Obj, "source_document_id");
  for (const t of ticketList.rows ?? []) link(`Weight ticket${text(t.ticket_number) ? " " + text(t.ticket_number) : ""}`, t, "source_document_id");
  // A package can list documents that no record points at; keep them visible.
  for (const x of listed) { const id = uuidText(x.id); if (id && !links.some((l) => l.doc?.id.toLowerCase() === id.toLowerCase())) links.push({ role: "Supporting document", state: "linked", doc: docRef(id) }); }

  const basisRaw = has(content, "applicable_rates_basis") ? content.applicable_rates_basis : null;
  const basis = typeof basisRaw === "string" && basisRaw.trim() !== "" ? basisRaw : null;
  const basisUnreadable = basisRaw !== null && basis === null;
  const applicable = applicableList.rows;
  const lineList = listOf(inv, "lines");
  const lines = lineList.rows;
  const pricedList = listOf(content, "priced_lines");
  const priced = pricedList.rows;

  return {
    ok: true,
    view: {
      summary: [
        field("Invoice", d, "invoice_number"),
        field("Vendor", d, "vendor_name"),
        field("Finding", d, "description"),
        field("Rule", d, "rule_id"),
        field("Finding amount", d, "amount", { money: true }),
        field("Expected amount", rec, "expected_amount", { money: true, nullText: withheld }),
        field("Invoiced amount", rec, "invoiced_amount", { money: true }),
        field("Discrepancy (billed − expected)", rec, "discrepancy_amount", { money: true, nullText: withheld }),
      ],
      invoice: {
        fields: [
          field("Invoice number", inv, "invoice_number"),
          field("Invoice date", inv, "invoice_date"),
          field("Service date", inv, "service_date"),
          field("Waste stream", inv, "waste_stream"),
          field("Invoice total", inv, "total", { money: true }),
        ],
        lines: lines && lines.map((l) => ({ description: cell(l, "description"), feeCode: cell(l, "fee_code", { nullText: "—" }), quantity: cell(l, "quantity"), unit: cell(l, "unit"), unitPrice: cell(l, "unit_price", { money: true }), lineTotal: cell(l, "line_total", { money: true }) })),
      },
      vendor: [field("Vendor name", d, "vendor_name"), field("Vendor ID", d, "vendor_id")],
      rates: { basisLabel: basis ? (BASIS[basis] ?? basis) : null, basisUnreadable, applicable: applicable && applicable.map(rateView), parent: parent ? rateView(parent) : null, parentUnreadable: parentSrc === "unreadable", parentUnconfirmed: parentStandsIn },
      manifest,
      calculations: {
        fields: [
          field("Pricing status", rec, "pricing_status"),
          field("Expected amount", rec, "expected_amount", { money: true, nullText: withheld }),
          field("Invoiced amount", rec, "invoiced_amount", { money: true }),
          field("Discrepancy (billed − expected)", rec, "discrepancy_amount", { money: true, nullText: withheld }),
          field("Partial subtotal (priced lines only, not a bill total)", rec, "partial_expected_amount", { money: true }),
          field("Total of identified findings", rec, "identified_finding_amount", { money: true, nullText: withheld }),
          field("Engine version", rec, "engine_version"),
          field("Reconciled at", rec, "created_at"),
        ].filter((f) => f.label.startsWith("Partial") ? (status === "PARTIAL" || f.state === "value") : true),
        pricedLines: priced && priced.map((p) => ({ description: cell(p, "description"), quantity: cell(p, "quantity"), unit: cell(p, "unit"), unitRate: cell(p, "unit_rate", { money: true }), expected: cell(p, "expected_line_amount", { money: true }), contract: cell(p, "contract_name") })),
      },
      discrepancy: [
        field("Rule", d, "rule_id"),
        field("Type", d, "discrepancy_type"),
        field("Description", d, "description"),
        field("Amount", d, "amount", { money: true }),
        field("Confidence", d, "confidence"),
        field("Review status", d, "review_status"),
        field("Reviewed at", d, "reviewed_at", { nullText: "Not reviewed yet" }),
        field("Review notes", d, "review_notes", { nullText: "No notes" }),
      ],
      trace,
      documents: links,
      issues: {
        invoiceLines: lineList.issue,
        authorizedFees: (applicable ?? (parent ? [parent] : [])).map((r) => listOf(r, "authorized_fees").issue).find((i) => i !== null) ?? null,
        weightTickets: ticketList.issue,
        applicableRates: applicableList.issue,
        pricedLines: pricedList.issue,
        sourceDocuments: docList.issue,
        trace: traceIssue,
      },
      objects: { discrepancy: d === "unreadable", reconciliation: rec === "unreadable", invoice: inv === "unreadable", contractRate: parentSrc === "unreadable", manifest: manifest.state === "unreadable" },
    },
  };
}

/**
 * Which package the review row should show. Once the persisted snapshot has been read back (it carries
 * created_at) it is the one displayed, even right after Generate; until then the package just generated
 * is shown. Never fetched or created here.
 */
export function selectEvidence(generated: unknown, stored: Record<string, unknown>, id: string): { evidence: unknown; source: "generated" | "stored" } | null {
  const s = stored[id];
  if (isObj(s) && typeof s.created_at === "string") return { evidence: s, source: "stored" };
  if (isObj(generated) && generated.discrepancy_id === id) return { evidence: generated, source: "generated" };
  if (isObj(s)) return { evidence: s, source: "stored" };
  return null;
}

/**
 * Latest-request-wins guard, one counter per key (a discrepancy id). begin() starts a request and returns
 * its token; a response may be applied only while isCurrent() is still true. invalidate() cancels every
 * request already in flight for that key (used when a newer result, such as a fresh POST, supersedes them).
 * release() cancels only the caller's own request (effect cleanup).
 */
export function createLatestGuard() {
  const seq = new Map<string, number>();
  return {
    begin(key: string): number { const n = (seq.get(key) ?? 0) + 1; seq.set(key, n); return n; },
    isCurrent(key: string, token: number): boolean { return seq.get(key) === token; },
    invalidate(key: string): void { seq.set(key, (seq.get(key) ?? 0) + 1); },
    /** Cancels ONLY the request that owns `token`: a no-op if a newer request (e.g. a Generate read-back) has begun since. */
    release(key: string, token: number): void { if (seq.get(key) === token) seq.set(key, token + 1); },
  };
}
