# Billing (Paddle)
**Checkout (frontend)**: Settings -> Billing -> Subscribe. The page fetches `GET /billing/checkout-context` (OWNER/ADMIN) which returns `organization_id`, `organization_sig` (HMAC of the id with the webhook secret) and the buyer's email, loads Paddle.js, and opens checkout with `customData = { organization_id, organization_sig }`. Public settings come from the web app's runtime env: `PADDLE_CLIENT_TOKEN`, `PADDLE_PRICE_ID`, `PADDLE_ENV`.

**Activation (backend)**: only `POST /api/v1/billing/paddle/webhook`, verified by HMAC over the raw body (`Paddle-Signature: ts=...;h1=...`, bounded timestamp, constant-time compare, secret rotation). Mapping: known `subscription id` first; otherwise `custom_data.organization_id` **only with a valid `organization_sig`**. Events are idempotent on event id, stale events are ignored, and an organization with an active subscription ignores events from a different subscription id. Entitled = active, trialing or past_due; `BILLING_ENFORCEMENT` gates reconciliation and monitoring server-side (402).

In Paddle, set the notification destination URL to `https://<host>/api/v1/billing/paddle/webhook`, subscribe to `subscription.*` events, and use the destination's secret as `PADDLE_WEBHOOK_SECRET`.

Not implemented: outbound Paddle API calls, customer portal / plan changes UI. Not verified against a real Paddle account (see BUILD_NOTES.md).
