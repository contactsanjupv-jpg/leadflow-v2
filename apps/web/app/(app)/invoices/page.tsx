"use client";
import Link from "next/link";
import { useState } from "react";
import { api } from "@/lib/api";
import { isValidDecimal } from "@/lib/money";
import DateInput from "@/components/DateInput";
import SourceDoc from "@/components/SourceDoc";
import { Badge, Card, Err, Money, useAction, useLoad } from "@/components/ui";

type Line = { description: string; fee_code: string; quantity: string; unit: string; unit_price: string; line_total: string };
const blank = (): Line => ({ description: "", fee_code: "", quantity: "1", unit: "", unit_price: "", line_total: "" });

export default function Invoices() {
  const list = useLoad("/invoices?limit=100");
  const vendors = useLoad("/vendors?limit=200");
  const manifests = useLoad("/manifests?limit=200");
  const act = useAction();
  const [f, setF] = useState({ vendor_id: "", invoice_number: "", invoice_date: "", service_date: "", waste_stream: "", manifest_id: "", total: "", source_document_id: "" });
  const [lines, setLines] = useState<Line[]>([blank()]);

  async function create(e: React.FormEvent) {
    e.preventDefault();
    const decimals = [f.total, ...lines.flatMap((l) => [l.quantity, l.unit_price, l.line_total])];
    if (decimals.some((d) => !isValidDecimal(d))) { await act.run(async () => { throw new Error("Amounts must be decimals with at most 4 places"); }); return; }
    const res = await act.run(() => api("POST", "/invoices", {
      vendor_id: f.vendor_id, invoice_number: f.invoice_number, invoice_date: f.invoice_date, service_date: f.service_date,
      waste_stream: f.waste_stream, manifest_id: f.manifest_id || null, source_document_id: f.source_document_id || null, total: f.total,
      line_items: lines.map((l) => ({ description: l.description, fee_code: l.fee_code || null, quantity: l.quantity, unit: l.unit, unit_price: l.unit_price, line_total: l.line_total })),
    }), "Invoice saved");
    if (res) { setLines([blank()]); setF({ ...f, invoice_number: "", total: "" }); void list.reload(); }
  }
  const setLine = (i: number, k: keyof Line, v: string) => setLines(lines.map((l, j) => (j === i ? { ...l, [k]: v } : l)));

  return (
    <>
      <h1>Invoices</h1>
      <Card title="Invoices">
        <Err e={list.error} />
        <table>
          <thead><tr><th>Invoice</th><th>Vendor</th><th>Service date</th><th className="num">Total</th><th>Status</th><th>Pricing</th><th className="num">Bill-level discrepancy</th></tr></thead>
          <tbody>
            {(list.data ?? []).map((i: any) => (
              <tr key={i.id}>
                <td><Link href={`/invoices/${i.id}`}>{i.invoice_number}</Link></td><td>{i.vendor_name}</td><td>{i.service_date}</td>
                <td className="num"><Money v={i.total} /></td><td>{i.review_status ? <Badge s={i.review_status} /> : <span className="muted">not reconciled</span>}</td>
                <td>{i.pricing_status ? <Badge s={i.pricing_status} /> : <span className="muted">not reconciled</span>}</td>
                <td className="num">{i.pricing_status === "COMPLETE" ? <Money v={i.discrepancy_amount} /> : <span className="muted">Not computed / not verified</span>}</td>
              </tr>
            ))}
            {list.data?.length === 0 && <tr><td colSpan={7} className="muted">No invoices yet. Enter one below.</td></tr>}
          </tbody>
        </table>
      </Card>
      <Card title="Enter an invoice">
        <p className="muted">Amounts are entered and stored as exact decimals. Link the manifest so reconciliation can check quantities and weights against it.</p>
        <form onSubmit={create}>
          <div className="row">
            <label>Vendor<select value={f.vendor_id} onChange={(e) => setF({ ...f, vendor_id: e.target.value })} required><option value="">Select…</option>{(vendors.data ?? []).map((v: any) => <option key={v.id} value={v.id}>{v.name}</option>)}</select></label>
            <label>Invoice #<input value={f.invoice_number} onChange={(e) => setF({ ...f, invoice_number: e.target.value })} required /></label>
            <label>Invoice date<DateInput value={f.invoice_date} onChange={(v) => setF({ ...f, invoice_date: v })} required /></label>
            <label>Service date<DateInput value={f.service_date} onChange={(v) => setF({ ...f, service_date: v })} required /></label>
            <label>Waste stream<input value={f.waste_stream} onChange={(e) => setF({ ...f, waste_stream: e.target.value })} required /></label>
            <label>Manifest<select value={f.manifest_id} onChange={(e) => setF({ ...f, manifest_id: e.target.value })}><option value="">None</option>{(manifests.data ?? []).map((m: any) => <option key={m.id} value={m.id}>{m.manifest_number}</option>)}</select></label>
            <SourceDoc value={f.source_document_id} onChange={(v) => setF({ ...f, source_document_id: v })} />
            <label>Invoice total<input value={f.total} onChange={(e) => setF({ ...f, total: e.target.value })} required inputMode="decimal" /></label>
          </div>
          <table>
            <thead><tr><th>Description</th><th>Fee code</th><th>Qty</th><th>Unit</th><th>Unit price</th><th>Line total</th><th /></tr></thead>
            <tbody>{lines.map((l, i) => (
              <tr key={i}>
                <td><input value={l.description} onChange={(e) => setLine(i, "description", e.target.value)} required /></td>
                <td><input value={l.fee_code} onChange={(e) => setLine(i, "fee_code", e.target.value)} placeholder="blank = disposal" size={12} /></td>
                <td><input value={l.quantity} onChange={(e) => setLine(i, "quantity", e.target.value)} size={6} required /></td>
                <td><input value={l.unit} onChange={(e) => setLine(i, "unit", e.target.value)} size={7} required /></td>
                <td><input value={l.unit_price} onChange={(e) => setLine(i, "unit_price", e.target.value)} size={9} required /></td>
                <td><input value={l.line_total} onChange={(e) => setLine(i, "line_total", e.target.value)} size={9} required /></td>
                <td>{lines.length > 1 && <button type="button" className="sec" onClick={() => setLines(lines.filter((_, j) => j !== i))}>✕</button>}</td>
              </tr>))}</tbody>
          </table>
          <div className="row"><button type="button" className="sec" onClick={() => setLines([...lines, blank()])}>Add line</button><button disabled={act.busy}>Save invoice</button></div>
          <Err e={act.error} />{act.ok && <div className="okmsg">{act.ok}</div>}
        </form>
      </Card>
    </>
  );
}