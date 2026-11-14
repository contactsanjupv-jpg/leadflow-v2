"use client";
import React, { useState } from "react"; // React in scope: node:test via tsx compiles JSX with the classic runtime
import type { ReactNode } from "react";
import { buildEvidenceView, hasEvidenceIssues, type Cell, type DocLink, type Field, type ListIssue, type RateView } from "../lib/evidence";

/** One value in one of its four states: recorded, recorded-as-empty, absent from the package, or present but malformed. */
function Val({ c }: { c: Cell }) {
  if (c.state === "value") return <span className={c.money ? "num" : undefined}>{c.value}</span>;
  if (c.state === "null") return <span className="muted">{c.nullText ?? "None recorded"}</span>;
  if (c.state === "unreadable") return <span className="err" data-testid="evidence-unreadable-value">Unreadable value (unexpected format)</span>;
  return <span className="muted">Not recorded in this package</span>;
}

const Note = ({ children }: { children: ReactNode }) => <p className="muted">{children}</p>;

/** A record that is present but not an object at all: flagged, and its values read as unreadable, not as missing. */
function ObjectAlert({ bad, what, id }: { bad: boolean; what: string; id: string }) {
  if (!bad) return null;
  return <p className="err" role="alert" data-testid={`evidence-object-issue-${id}`}>The {what} record could not be read (unexpected format). Its values are shown as unreadable, not as missing. Do not rely on this section as complete.</p>;
}

/** A list the reader could not fully use is flagged where it appears; it is never shown as if complete. */
function ListAlert({ issue, what, id }: { issue: ListIssue | null; what: string; id: string }) {
  if (!issue) return null;
  const msg = issue.kind === "unreadable"
    ? `Incomplete: the ${what} could not be read (unexpected format), so none are shown.`
    : `Incomplete: ${issue.skipped} of ${issue.total} ${what} could not be read and ${issue.skipped === 1 ? "is" : "are"} not shown.`;
  return <p className="err" role="alert" data-testid={`evidence-list-issue-${id}`}>{msg} Do not rely on this section as complete.</p>;
}

function Section({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return <section className="evsec" data-testid={`evidence-${id}`}><h3>{title}</h3>{children}</section>;
}

function Fields({ items }: { items: Field[] }) {
  return (
    <dl className="evgrid">
      {items.map((f) => <div key={f.label} className="evfield"><dt>{f.label}</dt><dd className={f.state === "value" && f.money ? "num" : undefined}><Val c={f} /></dd></div>)}
    </dl>
  );
}

function Scroll({ children }: { children: ReactNode }) {
  return <div className="evscroll">{children}</div>;
}

function Rate({ r, heading }: { r: RateView; heading: string }) {
  return (
    <div className="evrate">
      <h4>{heading}</h4>
      <Fields items={[...r.fields, { label: "Invoice lines priced by this rate", ...r.pricedLines }]} />
      <ListAlert issue={r.feesIssue} what="authorized fees" id="authorized-fees" />
      {r.fees === null ? (r.feesIssue ? null : <Note>Authorized fees are not recorded in this package.</Note>)
        : r.fees.length === 0 ? <Note>No authorized fees are listed for this rate.</Note>
        : <Scroll><table><thead><tr><th>Fee code</th><th>Description</th><th className="num">Amount</th><th>Authorization</th></tr></thead>
            <tbody>{r.fees.map((f, i) => <tr key={i}><td>{f.code.state === "value" ? f.code.value : <Val c={f.code} />}</td><td><Val c={f.description} /></td><td className="num"><Val c={f.amount} /></td><td>{f.typeLabel}</td></tr>)}</tbody></table></Scroll>}
    </div>
  );
}

function DocRow({ l }: { l: DocLink }) {
  return (
    <tr>
      <td>{l.role}</td>
      <td>
        {l.state === "linked" && l.doc && (l.doc.filename ?? <span className="muted">Document {l.doc.id.slice(0, 8)} (name not included in this package)</span>)}
        {l.state === "linked" && l.doc?.documentType && <div className="muted">{l.doc.documentType}</div>}
        {l.state === "none" && <span className="muted">No source document is linked</span>}
        {l.state === "absent" && <span className="muted">Not recorded in this package</span>}
        {l.state === "unreadable" && <span className="err" data-testid="evidence-unreadable-value">Unreadable source-document reference (unexpected format)</span>}
        {l.state === "linked" && l.doc?.detailsUnreadable && <div className="err" data-testid="evidence-unreadable-value">Some document details could not be read</div>}
      </td>
      <td className="evhash">{l.doc?.sha256 ? l.doc.sha256.slice(0, 12) + "…" : ""}</td>
      <td>{l.state === "linked" && l.doc && <a href={l.doc.href} download data-testid="evidence-source-document-link">Download original</a>}</td>
    </tr>
  );
}

/** The existing "Download PDF" action, unchanged in markup so existing tests and users are unaffected. */
export function EvidencePdfLink({ discrepancyId }: { discrepancyId: string }) {
  return <a className="linkbtn" href={`/api/proxy/discrepancies/${discrepancyId}/evidence/pdf`} download data-testid="evidence-pdf-link">Download PDF</a>;
}

export default function EvidencePackage({ evidence, source }: { evidence: unknown; source?: "generated" | "stored" }) {
  const [raw, setRaw] = useState(false);
  const result = buildEvidenceView(evidence);
  const rawBlock = (
    <div>
      <button type="button" className="sec" onClick={() => setRaw((v) => !v)} data-testid="evidence-raw-toggle">{raw ? "Hide raw JSON" : "Show raw JSON"}</button>
      {raw && <pre data-testid="evidence-raw-json">{JSON.stringify(evidence, null, 2)}</pre>}
    </div>
  );

  if (!result.ok) {
    return (
      <div className="evidence" data-testid="evidence-package">
        <p className="err" role="alert" data-testid="evidence-error">Evidence package unavailable. {result.reason}</p>
        {evidence !== undefined && evidence !== null && rawBlock}
      </div>
    );
  }
  const v = result.view;
  const createdAt = typeof (evidence as { created_at?: unknown })?.created_at === "string" ? (evidence as { created_at: string }).created_at : null;
  return (
    <div className="evidence" data-testid="evidence-package">
      <h3 className="evtitle">Evidence package</h3>
      {hasEvidenceIssues(v) && <p className="err" role="alert" data-testid="evidence-incomplete-banner">Part of this evidence package could not be read. Sections marked Incomplete or Unreadable are missing information: treat this package as incomplete.</p>}
      {source === "stored" && <Note><span data-testid="evidence-source-note">Stored snapshot{createdAt ? ` generated ${createdAt}` : ""}. It is shown unchanged; use Regenerate evidence package to create a new snapshot.</span></Note>}
      {source === "generated" && <Note><span data-testid="evidence-source-note">Snapshot generated just now.</span></Note>}
      <Section id="summary" title="Evidence summary">
        <ObjectAlert bad={v.objects.discrepancy} what="discrepancy" id="discrepancy" />
        <ObjectAlert bad={v.objects.reconciliation} what="reconciliation" id="reconciliation" />
        <Fields items={v.summary} />
      </Section>

      <Section id="invoice" title="Invoice details">
        <ObjectAlert bad={v.objects.invoice} what="invoice" id="invoice" />
        <Fields items={v.invoice.fields} />
        <ListAlert issue={v.issues.invoiceLines} what="invoice line items" id="invoice-lines" />
        {v.invoice.lines === null ? (v.issues.invoiceLines ? null : <Note>Invoice line items are not recorded in this package.</Note>)
          : v.invoice.lines.length === 0 ? <Note>No invoice line items are recorded in this package.</Note>
          : <Scroll><table><thead><tr><th>Description</th><th>Fee code</th><th className="num">Qty</th><th>Unit</th><th className="num">Unit price</th><th className="num">Line total</th></tr></thead>
              <tbody>{v.invoice.lines.map((l, i) => <tr key={i}><td><Val c={l.description} /></td><td><Val c={l.feeCode} /></td><td className="num"><Val c={l.quantity} /></td><td><Val c={l.unit} /></td><td className="num"><Val c={l.unitPrice} /></td><td className="num"><Val c={l.lineTotal} /></td></tr>)}</tbody></table></Scroll>}
      </Section>

      <Section id="vendor" title="Vendor details">
        <ObjectAlert bad={v.objects.discrepancy} what="discrepancy (it carries the vendor)" id="discrepancy-vendor" />
        <Fields items={v.vendor} />
      </Section>

      <Section id="rates" title="Contract and applicable rates">
        {v.rates.basisLabel && <Note>Applicable-rate basis: {v.rates.basisLabel}.</Note>}
        {v.rates.basisUnreadable && <p className="err" role="alert" data-testid="evidence-basis-unreadable">The applicable-rate basis could not be read (unexpected format).</p>}
        <ObjectAlert bad={v.rates.parentUnreadable} what="parent contract rate" id="contract-rate" />
        <ListAlert issue={v.issues.applicableRates} what="applicable rates" id="applicable-rates" />
        {v.rates.applicable !== null && v.rates.applicable.length > 0
          ? v.rates.applicable.map((r, i) => <Rate key={r.id ?? i} r={r} heading={`Applicable rate ${i + 1} of ${v.rates.applicable!.length}`} />)
          : v.rates.parent && v.rates.parentUnconfirmed ? (
            <div data-testid="evidence-parent-rate-unconfirmed">
              <Note>The applicable-rate list could not be read, so this parent contract rate is shown only as recorded in the package. It is NOT confirmed as the rate that applied.</Note>
              <Rate r={v.rates.parent} heading="Parent contract rate (not confirmed as an applicable rate)" />
            </div>)
          : v.rates.parent ? <Rate r={v.rates.parent} heading="Contract rate" />
          : v.issues.applicableRates?.kind === "unreadable" || v.rates.parentUnreadable ? null
          : <Note>{v.rates.applicable === null ? "Contract rates are not recorded in this package." : "No applicable rates are listed in this package."}</Note>}
      </Section>

      <Section id="manifest" title="Manifest and supporting records">
        <ListAlert issue={v.issues.weightTickets} what="weight tickets" id="weight-tickets" />
        {v.manifest.state === "absent" && <Note>The manifest is not recorded in this package.</Note>}
        {v.manifest.state === "none" && <Note>No manifest is linked to this reconciliation.</Note>}
        <ObjectAlert bad={v.manifest.state === "unreadable"} what="manifest" id="manifest" />
        {v.manifest.state === "present" && <>
          <Fields items={v.manifest.fields} />
          {v.manifest.tickets === null ? (v.issues.weightTickets ? null : <Note>Weight tickets are not recorded in this package.</Note>)
            : v.manifest.tickets.length === 0 ? <Note>No weight tickets are recorded for this manifest.</Note>
            : <Scroll><table><thead><tr><th>Ticket</th><th className="num">Net weight</th><th>Unit</th><th>Date</th></tr></thead>
                <tbody>{v.manifest.tickets.map((t, i) => <tr key={i}><td><Val c={t.number} /></td><td className="num"><Val c={t.netWeight} /></td><td><Val c={t.unit} /></td><td><Val c={t.date} /></td></tr>)}</tbody></table></Scroll>}
        </>}
      </Section>

      <Section id="calculations" title="Reconciliation calculations">
        <ObjectAlert bad={v.objects.reconciliation} what="reconciliation" id="reconciliation-calculations" />
        <Fields items={v.calculations.fields} />
        <ListAlert issue={v.issues.pricedLines} what="priced lines" id="priced-lines" />
        {v.calculations.pricedLines === null ? (v.issues.pricedLines ? null : <Note>Priced lines are not recorded in this package.</Note>)
          : v.calculations.pricedLines.length === 0 ? <Note>No priced lines are listed in this package.</Note>
          : <Scroll><table><thead><tr><th>Line</th><th className="num">Qty</th><th>Unit</th><th className="num">Rate</th><th className="num">Expected</th><th>Contract</th></tr></thead>
              <tbody>{v.calculations.pricedLines.map((p, i) => <tr key={i}><td><Val c={p.description} /></td><td className="num"><Val c={p.quantity} /></td><td><Val c={p.unit} /></td><td className="num"><Val c={p.unitRate} /></td><td className="num"><Val c={p.expected} /></td><td><Val c={p.contract} /></td></tr>)}</tbody></table></Scroll>}
      </Section>

      <Section id="discrepancy" title="Discrepancy details">
        <ObjectAlert bad={v.objects.discrepancy} what="discrepancy" id="discrepancy-details" />
        <Fields items={v.discrepancy} />
      </Section>

      <Section id="trace" title="Calculation trace">
        <ListAlert issue={v.issues.trace} what="calculation trace steps" id="trace" />
        {v.trace === null ? (v.issues.trace ? null : <Note>The calculation trace is not recorded in this package.</Note>)
          : v.trace.length === 0 ? <Note>The calculation trace is empty.</Note>
          : <ol className="evtrace">{v.trace.map((s, i) => <li key={i}>{s}</li>)}</ol>}
      </Section>

      <Section id="documents" title="Source documents">
        <ListAlert issue={v.issues.sourceDocuments} what="source documents" id="source-documents" />
        {v.documents.length === 0 ? (v.issues.sourceDocuments ? null : <Note>No source documents are referenced by this package.</Note>)
          : <Scroll><table><thead><tr><th>Used as</th><th>Document</th><th>SHA-256</th><th></th></tr></thead><tbody>{v.documents.map((l, i) => <DocRow key={l.role + i} l={l} />)}</tbody></table></Scroll>}
      </Section>

      {rawBlock}
    </div>
  );
}
