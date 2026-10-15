"use client";
import { useState } from "react";
import { useRouter } from "next/navigation";

export default function LoginPage() {
  const router = useRouter();
  const [mode, setMode] = useState<"login" | "signup">("login");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [org, setOrg] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    const body = mode === "login" ? { email, password } : { email, password, organization_name: org };
    const res = await fetch(`/api/auth/${mode}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    const data = await res.json().catch(() => null);
    setBusy(false);
    if (!res.ok) { setError(data?.error?.message ?? "Request failed"); return; }
    router.push("/"); router.refresh();
  }

  return (
    <div className="login card">
      <h1>XFlow</h1>
      <p className="muted">Continuous checking of hazardous-waste vendor invoices against your contracts.</p>
      <form onSubmit={submit}>
        {mode === "signup" && (
          <label>Organization<input value={org} onChange={(e) => setOrg(e.target.value)} required maxLength={200} /></label>
        )}
        <label style={{ marginTop: 8 }}>Email<input type="email" value={email} onChange={(e) => setEmail(e.target.value)} required autoComplete="username" /></label>
        <label style={{ marginTop: 8 }}>Password{mode === "signup" && " (12+ characters)"}
          <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} required minLength={mode === "signup" ? 12 : 1} autoComplete={mode === "login" ? "current-password" : "new-password"} />
        </label>
        {error && <div className="err" role="alert">{error}</div>}
        <div className="row" style={{ marginTop: 12 }}>
          <button disabled={busy}>{mode === "login" ? "Log in" : "Create account"}</button>
          <button type="button" className="sec" onClick={() => { setMode(mode === "login" ? "signup" : "login"); setError(null); }}>
            {mode === "login" ? "Create an account" : "I have an account"}
          </button>
        </div>
      </form>
    </div>
  );
}
