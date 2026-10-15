"use client";
import { useState } from "react";
import { api } from "@/lib/api";
import BillingCard from "@/components/BillingCard";
import { Badge, Card, Err, useAction, useLoad } from "@/components/ui";

export default function Settings() {
  const me = useLoad("/me");
  const members = useLoad("/members");
  const audit = useLoad("/audit?limit=50");
  const jobs = useLoad("/monitoring/jobs?limit=10");
  const [m, setM] = useState({ email: "", password: "", role: "MEMBER" });
  const act = useAction();
  const mon = useAction();

  async function add(e: React.FormEvent) {
    e.preventDefault();
    if (await act.run(() => api("POST", "/members", m), "Member added")) { setM({ email: "", password: "", role: "MEMBER" }); await members.reload(); }
  }
  return (
    <>
      <h1>Settings</h1>
      <BillingCard />
      <Card title="Continuous monitoring">
        <p className="muted">Monitoring re-checks every invoice that is new or whose contract rate, manifest or weight tickets changed, automatically on a schedule. You can also trigger a pass now.</p>
        <button disabled={mon.busy} onClick={async () => { await mon.run(() => api("POST", "/monitoring/run", {}), "Monitoring pass queued"); await jobs.reload(); }}>Run monitoring now</button>
        <Err e={mon.error} />{mon.ok && <div className="okmsg">{mon.ok}</div>}
        <table><thead><tr><th>Queued</th><th>Status</th><th>Attempts</th><th>Error</th></tr></thead>
          <tbody>{(jobs.data ?? []).map((j: any) => <tr key={j.id}><td>{new Date(j.created_at).toLocaleString()}</td><td><Badge s={j.status} /></td><td>{j.attempts}</td><td className="muted">{j.last_error}</td></tr>)}</tbody></table>
      </Card>
      <Card title="Team">
        {members.error ? <p className="muted">Member management is available to owners and admins.</p> : (
          <>
            <table><thead><tr><th>Email</th><th>Role</th></tr></thead><tbody>{(members.data ?? []).map((x: any) => <tr key={x.user_id}><td>{x.email}</td><td>{x.role}</td></tr>)}</tbody></table>
            <form className="row" onSubmit={add} style={{ marginTop: 10 }}>
              <label>Email<input type="email" value={m.email} onChange={(e) => setM({ ...m, email: e.target.value })} required /></label>
              <label>Initial password (12+)<input type="password" value={m.password} onChange={(e) => setM({ ...m, password: e.target.value })} required minLength={12} autoComplete="new-password" /></label>
              <label>Role<select value={m.role} onChange={(e) => setM({ ...m, role: e.target.value })}>{["MEMBER", "REVIEWER", "ADMIN", "OWNER"].map((r) => <option key={r}>{r}</option>)}</select></label>
              <button disabled={act.busy}>Add member</button>
            </form>
            <Err e={act.error} />{act.ok && <div className="okmsg">{act.ok}</div>}
          </>)}
      </Card>
      <Card title="Audit log">
        {audit.error ? <p className="muted">The audit log is available to owners and admins.</p> : (
          <table><thead><tr><th>When</th><th>Action</th><th>Resource</th></tr></thead>
            <tbody>{(audit.data ?? []).map((a: any) => <tr key={a.id}><td>{new Date(a.created_at).toLocaleString()}</td><td>{a.action}</td><td className="muted">{a.resource_type}</td></tr>)}</tbody></table>)}
      </Card>
    </>
  );
}
