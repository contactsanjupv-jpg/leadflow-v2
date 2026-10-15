import { cookies } from "next/headers";
import { NextResponse } from "next/server";
import { backendUrl, sameOrigin, SESSION_COOKIE } from "@/lib/server";

export async function POST(req: Request) {
  if (!sameOrigin(req)) return NextResponse.json({ error: { code: "FORBIDDEN", message: "cross-origin request blocked" } }, { status: 403 });
  const token = cookies().get(SESSION_COOKIE)?.value;
  if (token) {
    await fetch(`${backendUrl()}/api/v1/auth/logout`, { method: "POST", headers: { authorization: `Bearer ${token}` } }).catch(() => undefined);
  }
  const out = NextResponse.json({ ok: true });
  out.cookies.delete(SESSION_COOKIE);
  return out;
}
