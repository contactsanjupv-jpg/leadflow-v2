"use client";
import { useEffect, useState } from "react";
import { api } from "@/lib/api";
import { Badge, Card, Empty, Err, Money, useAction, useLoad } from "@/components/ui";

const FILTERS = ["", "NEW", "UNDER_REVIEW", "NEEDS_INFORMATION", "CONFIRMED", "DISPUTED", "REJECTED", "RESOLVED"];

export default function Review() {
  const [status, setStatus] = useState("NEW");
  const list = useLoad(`/discrepancies?limit=200${status ? `&status=${status}` : ""}`);
  const [open, setOpen] = useState<string | null>(null);
  const [notes, setNotes] = useState("");
  const [evidence, setEvidence] = useState<any>(null);
  const act = useAction();
  const [hasEvidence, setHasEvidence] = useState<Record<string, boolean>>({});
  useEffect(() => {
    if (!open || hasEvidence[open] !== undefined) return;
    api("GET", `/discrepancies/${open}/evidence`).then(() => setHasEvidence((h) => ({ ...h, [open]: true }))).catch(() => setHasEvidence((h) => ({ ...h, [open]: false })));
  }, [open, hasEvidence]);

  async function review(id: string, action: string) {
    const r = await act.run(() => api("POST", `/discrepancies/${id}/review`, { action, notes: notes || undefined }), "Saved");
    if (r) { setNotes(""); await list.reload(); }
  }
  async function makeEvidence(id: string) {
    const r = await act.run(() => api("POST", `/discrepancies/${id}/evidence`, {}), "Evidence package generated");
    if (r) { setEvidence(r); setHasEvidence((h) => ({ ...h, [id]: true })); }
  }
  async function dispute(id: string) {
    const r = await act.run(() => api("POST", "/disputes", { discrepancy_id: id }), "Dispute opened — see Disputes & recovery");
    if (r) await list.reload();
  }

  return (
    <>
      <h1>Review queue</h1>
      <Card>
        <div className="row"><label>Status<select value={status} onChange={(e) => { setStatus(e.target.value); setOpen(null); }}>{FILTERS.map((s) => <option key={s} value={s}>{s || "All"}</option>)}</select></label></div>
        <Err e={list.error} /><Err e={act.error} />{act.ok && <div className="okmsg">{act.ok}</div>}
        {list.data?.length === 0 && <Empty>No discrepancies with this status.</Empty>}
        <table>
          <thead><tr><th>Invoice</th><th>Vendor</th><th>Finding</th><th>Confidence</th><th className="num">Amount</th><th>Status</th></tr></thead>
          <tbody>
            {(list.data ?? []).flatMap((d: any) => [
              <tr key={d.id} onClick={() => { setOpen(open === d.id ? null : d.id); setEvidence(null); }} style={{ cursor: "pointer" }}>
                <td>{d.invoice_number}</td><td>{d.vendor_name}</td><td>{d.description}<div className="muted">{d.rule_id}</div></td><td>{d.confidence}</td>
                <td className="num"><Money v={d.amount} /></td><td><Badge s={d.review_status} /></td>
              </tr>,
              open === d.id && (
                <tr key={d.id + "-x"}><td colSpan={6}>
                  <div className="row"><label style={{ flex: 1 }}>Notes (required to reject / request information)<textarea rows={2} value={notes} onChange={(e) => setNotes(e.target.value)} /></label></div>
                  <div className="row">
                    {["NEW", "NEEDS_INFORMATION"].includes(d.review_status) && <button className="sec" onClick={() => review(d.id, "start_review")} disabled={act.busy}>Start review</button>}
                    {["NEW", "UNDER_REVIEW", "NEEDS_INFORMATION"].includes(d.review_status) && <>
                      <button onClick={() => review(d.id, "confirm")} disabled={act.busy}>Confirm</button>
                      <button className="danger" onClick={() => review(d.id, "reject")} disabled={act.busy || !notes.trim()}>Reject</button>
                      <button className="sec" onClick={() => review(d.id, "needs_information")} disabled={act.busy || !notes.trim()}>Needs information</button></>}
                    {["CONFIRMED", "REJECTED"].includes(d.review_status) && <button className="sec" onClick={() => review(d.id, "reopen")} disabled={act.busy}>Reopen</button>}
                    <button className="sec" onClick={() => makeEvidence(d.id)} disabled={act.busy}>{hasEvidence[d.id] ? "Regenerate evidence package" : "Generate evidence package"}</button>
                    {hasEvidence[d.id] && <a className="linkbtn" href={`/api/proxy/discrepancies/${d.id}/evidence/pdf`} download data-testid="evidence-pdf-link">Download PDF</a>}
                    {d.review_status === "CONFIRMED" && <button onClick={() => dispute(d.id)} disabled={act.busy}>Open dispute</button>}
                  </div>
                  {d.review_notes && <p className="muted">Notes: {d.review_notes}</p>}
                  {evidence && evidence.discrepancy_id === d.id && <details open><summary>Evidence package</summary><pre>{JSON.stringify(evidence.content, null, 2)}</pre></details>}
                </td></tr>
              ),
            ])}
          </tbody>
        </table>
      </Card>
    </>
  );
}
