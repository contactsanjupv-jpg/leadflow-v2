"use client";
import { useState } from "react";
import { api } from "@/lib/api";
import { isValidDecimal } from "@/lib/money";
import { Badge, Card, Empty, Err, Money, useAction, useLoad } from "@/components/ui";

// Next legal step(s) per status; mirrors the server state machine (the server is authoritative).
const NEXT: Record<string, string[]> = {
  OPEN: ["PREPARE", "CLOSED"], PREPARE: ["REVIEW", "CLOSED"], REVIEW: ["SUBMITTED", "PREPARE", "CLOSED"], SUBMITTED: ["VENDOR_RESPONDED", "CLOSED"],
  VENDOR_RESPONDED: ["CREDIT_PROMISED", "CLOSED"], CREDIT_PROMISED: ["CREDIT_RECEIVED", "CLOSED"], CREDIT_RECEIVED: ["VERIFIED"], VERIFIED: ["CLOSED"], CLOSED: [],
};

export default function Disputes() {
  const list = useLoad("/disputes?limit=100");
  const rec = useLoad("/recovery");
  const [open, setOpen] = useState<string | null>(null);
  const detail = useLoad(open ? `/disputes/${open}` : null);
  const [amount, setAmount] = useState("");
  const [memo, setMemo] = useState("");
  const act = useAction();

  async function go(id: string, to: string) {
    if ((to === "CREDIT_PROMISED" || to === "CREDIT_RECEIVED") && !isValidDecimal(amount)) { await act.run(async () => { throw new Error("Enter the amount as a decimal, e.g. 500.00"); }); return; }
    const body: any = { to };
    if (to === "CREDIT_PROMISED" || to === "CREDIT_RECEIVED") body.amount = amount;
    if (to === "CREDIT_RECEIVED") body.credit_memo_reference = memo;
    const r = await act.run(() => api("POST", `/disputes/${id}/transition`, body), `Moved to ${to.replaceAll("_", " ")}`);
    if (r) { setAmount(""); setMemo(""); await Promise.all([list.reload(), detail.reload(), rec.reload()]); }
  }
  const r = rec.data, d = detail.data;
  return (
    <>
      <h1>Disputes & recovery</h1>
      <Card title="Recovery summary">
        <Err e={rec.error} />
        <div className="grid">{[["Identified", r?.identified], ["Confirmed", r?.confirmed], ["Recovered (verified)", r?.recovered_verified], ["Received, unverified", r?.received_pending_verification], ["Unrecovered", r?.unrecovered]].map(([l, v]) => (
          <div className="stat" key={l as string}><div className="v"><Money v={v as string} /></div><div className="l">{l}</div></div>))}</div>
      </Card>
      <Card title="Disputes">
        <Err e={list.error} /><Err e={act.error} />{act.ok && <div className="okmsg">{act.ok}</div>}
        {list.data?.length === 0 && <Empty>No disputes yet. Confirm a discrepancy in the review queue, then open a dispute.</Empty>}
        <table><thead><tr><th>Vendor</th><th>Status</th><th className="num">Disputed</th><th className="num">Vendor credit</th></tr></thead>
          <tbody>{(list.data ?? []).flatMap((x: any) => [
            <tr key={x.id} style={{ cursor: "pointer" }} onClick={() => setOpen(open === x.id ? null : x.id)}><td>{x.vendor_name}</td><td><Badge s={x.status} /></td><td className="num"><Money v={x.disputed_amount} /></td><td className="num"><Money v={x.confirmed_amount} /></td></tr>,
            open === x.id && d && (
              <tr key={x.id + "-d"}><td colSpan={4}>
                {d.credit && <p className="muted">Identified <Money v={d.credit.identified_amount} /> · promised <Money v={d.credit.confirmed_amount} /> · received <Money v={d.credit.recovered_amount} />{d.credit.credit_memo_reference ? ` (memo ${d.credit.credit_memo_reference})` : ""}{d.credit.verified_at ? " · verified" : ""}</p>}
                {(d.status === "VENDOR_RESPONDED" || d.status === "CREDIT_PROMISED") && (
                  <div className="row"><label>{d.status === "VENDOR_RESPONDED" ? "Credit promised by vendor" : "Credit actually received"}<input value={amount} onChange={(e) => setAmount(e.target.value)} inputMode="decimal" placeholder="0.00" /></label>
                    {d.status === "CREDIT_PROMISED" && <label>Credit memo reference<input value={memo} onChange={(e) => setMemo(e.target.value)} /></label>}</div>)}
                <div className="row">{NEXT[d.status].map((to) => (
                  <button key={to} className={to === "CLOSED" ? "sec" : ""} disabled={act.busy} onClick={() => go(d.id, to)}>{to === "CLOSED" ? "Close" : `→ ${to.replaceAll("_", " ")}`}</button>))}
                  {NEXT[d.status].length === 0 && <span className="muted">Closed.</span>}</div>
                {d.status === "CREDIT_RECEIVED" && <p className="muted">Verifying confirms the credit really appeared on your account; it is restricted to owners and admins.</p>}
              </td></tr>
            ),
          ])}</tbody></table>
      </Card>
    </>
  );
}
