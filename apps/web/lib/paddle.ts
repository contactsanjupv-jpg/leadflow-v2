// Paddle Billing checkout helpers (Paddle.js v2). Pure functions are unit-tested; the DOM loader is not.
export const PADDLE_SCRIPT_URL = "https://cdn.paddle.com/paddle/v2/paddle.js";

export type CheckoutConfig = { configured: boolean; environment?: "sandbox" | "production"; token?: string; priceId?: string };
// Issued by the backend (owner/admin only). `organization_sig` proves the organization link so the webhook
// can trust `custom_data.organization_id`; a checkout without it cannot change anyone's subscription.
export type CheckoutContext = { organization_id: string; organization_sig: string; email: string };

export function buildCheckoutOptions(cfg: CheckoutConfig, ctx: CheckoutContext) {
  if (!cfg.configured || !cfg.priceId) throw new Error("Billing is not configured on this server");
  return {
    items: [{ priceId: cfg.priceId, quantity: 1 }],
    customer: { email: ctx.email },
    customData: { organization_id: ctx.organization_id, organization_sig: ctx.organization_sig },
  };
}

/** Subscription states that unlock processing (mirrors the backend rule; the backend is authoritative). */
export function isEntitledStatus(status: string | undefined | null): boolean {
  return status === "active" || status === "trialing" || status === "past_due";
}

export function loadPaddleScript(): Promise<any> {
  const w = window as any;
  if (w.Paddle) return Promise.resolve(w.Paddle);
  return new Promise((resolve, reject) => {
    const s = document.createElement("script");
    s.src = PADDLE_SCRIPT_URL;
    s.async = true;
    s.onload = () => (w.Paddle ? resolve(w.Paddle) : reject(new Error("Paddle.js loaded but is unavailable")));
    s.onerror = () => reject(new Error("Could not load Paddle.js (blocked by the network or an ad blocker?)"));
    document.head.appendChild(s);
  });
}
