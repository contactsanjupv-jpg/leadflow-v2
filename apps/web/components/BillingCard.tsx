"use client";
import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/api";
import { buildCheckoutOptions, isEntitledStatus, loadPaddleScript, type CheckoutConfig, type CheckoutContext } from "@/lib/paddle";
import { Card, Err, useLoad } from "@/components/ui";

export default function BillingCard() {
  const status = useLoad("/billing/status");
  const [cfg, setCfg] = useState<CheckoutConfig | null>(null);
  const [busy, setBusy] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const initialized = useRef(false);
  const timer = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    fetch("/api/billing/config").then((r) => r.json()).then(setCfg).catch(() => setCfg({ configured: false }));
    return () => { if (timer.current) clearInterval(timer.current); };
  }, []);

  // The subscription is activated by Paddle's signed webhook, not by this page. After checkout we only
  // WAIT for that to land (poll the server-side status), then refresh so the entitlement banner updates.
  function waitForActivation() {
    setWaiting(true);
    let tries = 0;
    timer.current = setInterval(async () => {
      tries += 1;
      try {
        const s = await api<{ status: string }>("GET", "/billing/status");
        if (isEntitledStatus(s.status)) {
          if (timer.current) clearInterval(timer.current);
          window.location.reload();
          return;
        }
      } catch { /* keep polling */ }
      if (tries >= 45) {
        if (timer.current) clearInterval(timer.current);
        setWaiting(false);
        setError("Payment received but activation is taking longer than expected. Refresh in a minute; if it persists, contact support.");
      }
    }, 2000);
  }

  async function subscribe() {
    setBusy(true); setError(null);
    try {
      if (!cfg?.configured || !cfg.token) throw new Error("Billing is not configured on this server");
      const ctx = await api<CheckoutContext>("GET", "/billing/checkout-context");
      const Paddle = await loadPaddleScript();
      if (!initialized.current) {
        if (cfg.environment === "sandbox") Paddle.Environment.set("sandbox");
        Paddle.Initialize({ token: cfg.token, eventCallback: (e: any) => { if (e?.name === "checkout.completed") waitForActivation(); } });
        initialized.current = true;
      }
      Paddle.Checkout.open(buildCheckoutOptions(cfg, ctx));
    } catch (e: any) {
      setError(e.status === 403 ? "Only owners and admins can manage the subscription." : e.message ?? "Could not start checkout");
    } finally { setBusy(false); }
  }

  const s = status.data;
  const entitled = isEntitledStatus(s?.status);
  return (
    <Card title="Billing">
      <Err e={status.error} />
      {s && <p data-testid="billing-status">Subscription status: <b>{s.status}</b>{s.plan ? ` · ${s.plan}` : ""}{s.current_period_end ? ` · renews ${new Date(s.current_period_end).toLocaleDateString()}` : ""}</p>}
      {!entitled && (
        <>
          <p className="muted">Subscribing unlocks reconciliation and continuous monitoring. Checkout is handled by Paddle; your subscription activates when Paddle confirms the payment.</p>
          <button onClick={subscribe} disabled={busy || waiting || cfg === null}>{waiting ? "Waiting for activation…" : "Subscribe"}</button>
          {cfg && !cfg.configured && <p className="muted">Billing is not configured on this server.</p>}
        </>
      )}
      {entitled && <p className="okmsg">Your subscription is active.</p>}
      <Err e={error} />
    </Card>
  );
}
