"use client";
import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/api";
import { Badge, Card, Empty, Err, Money, useAction, useLoad } from "@/components/ui";
import EvidencePackage, { EvidencePdfLink } from "@/components/EvidencePackage";
import { createLatestGuard, selectEvidence } from "@/lib/evidence";

const FILTERS = ["", "NEW", "UNDER_REVIEW", "NEEDS_INFORMATION", "CONFIRMED", "DISPUTED", "REJECTED", "RESOLVED"];

export default function Review() {
  const [status, setStatus] = useState("NEW");
  const list = useLoad(`/discrepancies?limit=200${status ? `&status=${status}` : ""}`);
  const [open, setOpen] = useState<string | null>(null);
  const [notes, setNotes] = useState("");
  const [evidence, setEvidence] = useState<any>(null);
  // Snapshots already stored for a discrepancy, fetched read-only (GET) when its row is opened.
  const [stored, setStored] = useState<Record<string, unknown>>({});
  const act = useAction();
  const [hasEvidence, setHasEvidence] = useState<Record<string, boolean>>({});
  // Latest-request-wins per discrepancy: a slow response for an earlier row selection (or one that a newer
  // generate has superseded) must not overwrite newer state.
  const latest = useRef(createLatestGuard());
  useEffect(() => {
    if (!open || hasEvidence[open] !== undefined) return;
    const id = open;
    const token = latest.current.begin(id);
    api("GET", `/discrepancies/${id}/evidence`)
      .then((r) => { if (!latest.current.isCurrent(id, token)) return; setStored((s) => ({ ...s, [id]: r })); setHasEvidence((h) => ({ ...h, [id]: true })); })
      .catch(() => { if (!latest.current.isCurrent(id, token)) return; setHasEvidence((h) => ({ ...h, [id]: false })); });
    // Cleanup (effect re-run or unmount) cancels only THIS effect's own request. It must not use the row-wide invalidate: a
    // newer Generate read-back for the same row owns a newer token and has to survive.
    return () => latest.current.release(id, token);
  }, [open, hasEvidence]);

  async function review(id: string, action: string) {
    const r = await act.run(() => api("POST", `/discrepancies/${id}/review`, { action, notes: notes || undefined }), "Saved");
    if (r) { setNotes(""); await list.reload(); }
  }
  async function makeEvidence(id: string) {
    const r = await act.run(() => api("POST", `/discrepancies/${id}/evidence`, {}), "Evidence package generated");
    if (r) {
      // This POST supersedes any read already in flight, and replaces an older stored snapshot, for this row.
      latest.current.invalidate(id);
      setEvidence(r); setHasEvidence((h) => ({ ...h, [id]: true })); setStored((s) => ({ ...s, [id]: r }));
      // Read the persisted snapshot back; once it arrives (with its real created_at) it is the one displayed.
      const token = latest.current.begin(id);
      api("GET", `/discrepancies/${id}/evidence`).then((g) => { if (latest.current.isCurrent(id, token)) setStored((s) => ({ ...s, [id]: g })); }).catch(() => {});
    }
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
                    {hasEvidence[d.id] && <EvidencePdfLink discrepancyId={d.id} />}
                    {d.review_status === "CONFIRMED" && <button onClick={() => dispute(d.id)} disabled={act.busy}>Open dispute</button>}
                  </div>
                  {d.review_notes && <p className="muted">Notes: {d.review_notes}</p>}
                  {(() => { const shown = selectEvidence(evidence, stored, d.id); return shown && <EvidencePackage evidence={shown.evidence} source={shown.source} />; })()}
                </td></tr>
              ),
            ])}
          </tbody>
        </table>
      </Card>
    </>
  );
}
