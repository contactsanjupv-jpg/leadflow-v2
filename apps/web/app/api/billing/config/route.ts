import { cookies } from "next/headers";
import { NextResponse } from "next/server";
import { SESSION_COOKIE } from "@/lib/server";

// Public Paddle checkout settings, read from RUNTIME env (so one image works for sandbox and production).
// The client-side token and price id are not secrets, but are still only served to signed-in users.
export async function GET() {
  if (!cookies().get(SESSION_COOKIE)?.value) return NextResponse.json({ error: { code: "AUTHENTICATION_ERROR", message: "authentication required" } }, { status: 401 });
  const token = process.env.PADDLE_CLIENT_TOKEN;
  const priceId = process.env.PADDLE_PRICE_ID;
  const environment = process.env.PADDLE_ENV === "production" ? "production" : "sandbox";
  return NextResponse.json({ configured: Boolean(token && priceId), environment, token, priceId });
}
export const dynamic = "force-dynamic";
