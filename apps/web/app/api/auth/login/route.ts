import { NextResponse } from "next/server";
import { backendUrl, cookieOptions, sameOrigin, SESSION_COOKIE } from "@/lib/server";

export async function POST(req: Request) {
  if (!sameOrigin(req)) return NextResponse.json({ error: { code: "FORBIDDEN", message: "cross-origin request blocked" } }, { status: 403 });
  const body = await req.text();
  const res = await fetch(`${backendUrl()}/api/v1/auth/login`, { method: "POST", headers: { "content-type": "application/json" }, body });
  const data = await res.json().catch(() => null);
  if (!res.ok) return NextResponse.json(data ?? { error: { code: "ERROR", message: "request failed" } }, { status: res.status });
  const out = NextResponse.json({ ok: true });
  out.cookies.set(SESSION_COOKIE, data.session_token, cookieOptions(60 * 60 * 24 * 7));
  return out;
}
