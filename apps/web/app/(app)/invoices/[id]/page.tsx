"use client";
import Link from "next/link";
import { useParams } from "next/navigation";
import { api } from "@/lib/api";
import { Badge, Card, Err, Money, useAction, useLoad } from "@/components/ui";

export default function InvoiceDetail() {
  const { id } = useParams<{ id: string }>();
  const inv = useLoad(`/invoices/${id}`);
  const recId = inv.data?.latest_reconciliation?.id as string | undefined;
  const rec = useLoad(recId ? `/reconciliations/${recId}` : null);
  const act = useAction();

  async function run() {
    await act.run(() => api("POST", "/reconciliations/run", { invoice_id: id }), "Reconciliation complete");
    await inv.reload();
  }
  const i = inv.data, r = rec.data;
  return (
    <>
      <h1>Invoice {i?.invoice_number ?? ""}</h1>
      <Err e={inv.error} />
      {i && (
        <>
          <Card>
            <div className="row">
              <div>Service date <b>{i.service_date}</b></div><div>Waste stream <b>{i.waste_stream}</b></div>
              <div>Total <b><Money v={i.total} /></b></div>
              <div>Manifest {i.manifest_id ? <Link href="/data">linked</Link> : <span className="muted">none linked</span>}</div>
            </div>
            <table><thead><tr><th>Description</th><th>Fee code</th><th className="num">Qty</th><th>Unit</th><th className="num">Unit price</th><th className="num">Line total</th></tr></thead>
              <tbody>{i.lines.map((l: any) => <tr key={l.id}><td>{l.description}</td><td>{l.fee_code ?? ""}</td><td className="num">{l.quantity}</td><td>{l.unit}</td><td className="num"><Money v={l.unit_price} /></td><td className="num"><Money v={l.line_total} /></td></tr>)}</tbody></table>
            <div className="row" style={{ marginTop: 10 }}><button onClick={run} disabled={act.busy}>{r ? "Re-run reconciliation" : "Run reconciliation"}</button></div>
            <Err e={act.error} />{act.ok && <div className="okmsg">{act.ok}</div>}
          </Card>
          {r && (
            <Card title="Latest reconciliation">
              <div className="row"><div>Expected <b><Money v={r.expected_amount} /></b></div><div>Invoiced <b><Money v={r.invoiced_amount} /></b></div><div>Discrepancy <b><Money v={r.discrepancy_amount} /></b></div><Badge s={r.review_status} /></div>
              <table><thead><tr><th>Finding</th><th>Rule</th><th className="num">Amount</th><th>Status</th></tr></thead>
                <tbody>{r.discrepancies.map((d: any) => <tr key={d.id}><td><Link href="/review">{d.description}</Link></td><td className="muted">{d.rule_id}</td><td className="num"><Money v={d.amount} /></td><td><Badge s={d.review_status} /></td></tr>)}
                  {r.discrepancies.length === 0 && <tr><td colSpan={4} className="muted">No discrepancies found.</td></tr>}</tbody></table>
              <details><summary>Calculation trace</summary><pre>{(r.calculation_trace as string[]).join("\n")}</pre></details>
            </Card>
          )}
        </>
      )}
    </>
  );
}
