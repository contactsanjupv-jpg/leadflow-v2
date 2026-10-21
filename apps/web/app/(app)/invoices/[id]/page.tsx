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
    await rec.reload();
  }
  const i = inv.data, r = rec.data;
  const priced = (r?.priced_lines ?? []) as Array<{
    invoice_line_item_id: string;
    contract_rate_id: string;
    expected_line_amount: string;
    unit: string;
    unit_rate: string;
  }>;
  const lineById = new Map((i?.lines ?? []).map((l: { id: string }) => [l.id, l]));

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
              <div className="row">
                <div>Expected <b>{r.pricing_status === "COMPLETE" ? <Money v={r.expected_amount} /> : "Not computed / not verified"}</b></div>
                <div>Invoiced <b><Money v={r.invoiced_amount} /></b></div>
                <div>{r.pricing_status === "COMPLETE" ? "Bill-level discrepancy" : "Identified finding amount"} <b><Money v={r.pricing_status === "COMPLETE" ? r.discrepancy_amount : r.identified_finding_amount} /></b></div>
                <Badge s={r.review_status} />
                <Badge s={r.pricing_status} />
              </div>
              {r.pricing_status === "PARTIAL" && <p className="muted">Partial expected subtotal: <b><Money v={r.partial_expected_amount} /></b>. This is only the amount that could be priced; it is not the full invoice expected amount. The bill-level discrepancy is not computed until the unit mismatch is resolved.</p>}
              {r.pricing_status === "UNCOMPUTABLE" && <p className="muted">No contract rate effective on the service date was available. The expected amount and bill-level discrepancy cannot be computed. Resolve rate coverage and rerun reconciliation.</p>}
              {r.pricing_status === "LEGACY_UNVERIFIED" && <p className="muted">This is a historical reconciliation created before pricing completeness was tracked. Its prior expected amount and bill-level discrepancy are not verified; rerun reconciliation to calculate them under the current rules.</p>}
              <h2>Priced lines</h2>
              <table>
                <thead><tr><th>Line</th><th>Contract unit</th><th className="num">Contract rate</th><th className="num">Expected amount</th></tr></thead>
                <tbody>
                  {priced.map((p) => {
                    const line = lineById.get(p.invoice_line_item_id) as { description?: string; unit?: string } | undefined;
                    return (
                      <tr key={p.invoice_line_item_id}>
                        <td>{line?.description ?? p.invoice_line_item_id}</td>
                        <td>{p.unit}</td>
                        <td className="num"><Money v={p.unit_rate} />/{p.unit}</td>
                        <td className="num"><Money v={p.expected_line_amount} /></td>
                      </tr>
                    );
                  })}
                  {priced.length === 0 && <tr><td colSpan={4} className="muted">No persisted line-level rates for this reconciliation.</td></tr>}
                </tbody>
              </table>
              <table><thead><tr><th>Finding</th><th>Rule</th><th className="num">Amount</th><th>Status</th></tr></thead>
                <tbody>{r.discrepancies.map((d: any) => <tr key={d.id}><td><Link href="/review">{d.description}</Link></td><td className="muted">{d.rule_id}</td><td className="num"><Money v={d.amount} /></td><td><Badge s={d.review_status} /></td></tr>)}
                  {r.discrepancies.length === 0 && <tr><td colSpan={4} className="muted">No discrepancies found.</td></tr>}</tbody></table>
              <details><summary>{r.pricing_status === "LEGACY_UNVERIFIED" ? "Historical calculation trace (unverified)" : "Calculation trace"}</summary><pre>{(r.calculation_trace as string[]).join("\n")}</pre></details>
            </Card>
          )}
        </>
      )}
    </>
  );
}