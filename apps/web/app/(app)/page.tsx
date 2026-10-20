"use client";
import Link from "next/link";
import { Card, Err, Money, useLoad } from "@/components/ui";

export default function Dashboard() {
  const dash = useLoad("/dashboard");
  const rec = useLoad("/recovery");
  const bill = useLoad("/billing/status");
  const notes = useLoad("/notifications?limit=5");
  const d = dash.data, r = rec.data;
  return (
    <>
      <h1>Dashboard</h1>
      <Err e={dash.error} />
      <div className="grid">
        <Card><div className="stat"><div className="v">{d?.invoices_analyzed ?? "—"}</div><div className="l">Invoices analyzed</div></div></Card>
        <Card><div className="stat"><div className="v">{d?.requiring_review ?? "—"}</div><div className="l">Requiring review</div></div></Card>
        <Card><div className="stat"><div className="v"><Money v={d?.potential_discrepancy_total} /></div><div className="l">Identified finding amounts (unreviewed)</div></div></Card>
        <Card><div className="stat"><div className="v"><Money v={d?.confirmed_discrepancy_total} /></div><div className="l">Confirmed / disputed finding amounts</div></div></Card>
        <Card><div className="stat"><div className="v">{d?.pricing_incomplete_count ?? "—"}</div><div className="l">Latest reconciliations with incomplete / unverified pricing</div></div></Card>
      </div>
      <Card title="Recovery">
        <Err e={rec.error} />
        <div className="grid">
          {[["Identified", r?.identified], ["Confirmed by vendor", r?.confirmed], ["Recovered (verified)", r?.recovered_verified],
            ["Received, awaiting verification", r?.received_pending_verification], ["Unrecovered", r?.unrecovered]].map(([l, v]) => (
            <div className="stat" key={l as string}><div className="v"><Money v={v as string} /></div><div className="l">{l}</div></div>
          ))}
        </div>
        <p className="muted">Only credits a person has verified count as recovered. <Link href="/disputes">Manage disputes</Link></p>
      </Card>
      <div className="grid">
        <Card title="Subscription">
          <Err e={bill.error} />
          {bill.data && <p>Status: <b>{bill.data.status}</b>{bill.data.plan ? ` · plan ${bill.data.plan}` : ""}{bill.data.current_period_end ? ` · renews ${new Date(bill.data.current_period_end).toLocaleDateString()}` : ""}</p>}
        </Card>
        <Card title="Recent notifications">
          {(notes.data ?? []).length === 0 && <p className="muted">Nothing yet.</p>}
          {(notes.data ?? []).map((n: any) => (
            <p key={n.id}><b>{n.event_type}</b> — {n.payload?.discrepancy_count} finding(s), identified finding amount <Money v={n.payload?.identified_finding_amount} />; complete-priced discrepancy total <Money v={n.payload?.complete_discrepancy_amount} />; {n.payload?.incomplete_pricing_invoice_count === undefined ? "pricing completeness not recorded" : `${n.payload.incomplete_pricing_invoice_count} invoice(s) with incomplete pricing`} <span className="muted">{new Date(n.created_at).toLocaleString()}</span></p>
          ))}
        </Card>
      </div>
    </>
  );
}
