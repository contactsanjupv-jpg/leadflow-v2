import { cookies } from "next/headers";
import { NextResponse } from "next/server";
import { backendUrl, sameOrigin, SESSION_COOKIE } from "@/lib/server";

// Authenticated pass-through to the Rust API. Only /api/v1/* is reachable, the session token is
// injected from the httpOnly cookie, and state-changing calls must be same-origin (CSRF).
async function forward(req: Request, { params }: { params: { path: string[] } }) {
  if (!sameOrigin(req)) return NextResponse.json({ error: { code: "FORBIDDEN", message: "cross-origin request blocked" } }, { status: 403 });
  const token = cookies().get(SESSION_COOKIE)?.value;
  if (!token) return NextResponse.json({ error: { code: "AUTHENTICATION_ERROR", message: "authentication required" } }, { status: 401 });
  if (params.path.some((p) => p === ".." || p === "." || p.includes("\\"))) return NextResponse.json({ error: { code: "NOT_FOUND", message: "not found" } }, { status: 404 });
  if (params.path[0] === "billing" && params.path[1] === "paddle") return NextResponse.json({ error: { code: "NOT_FOUND", message: "not found" } }, { status: 404 });

  const url = new URL(req.url);
  const target = `${backendUrl()}/api/v1/${params.path.map(encodeURIComponent).join("/")}${url.search}`;
  const headers: Record<string, string> = { authorization: `Bearer ${token}` };
  const ct = req.headers.get("content-type");
  if (ct) headers["content-type"] = ct;
  const hasBody = !["GET", "HEAD"].includes(req.method);
  const upstream = await fetch(target, { method: req.method, headers, body: hasBody ? await req.arrayBuffer() : undefined, cache: "no-store" });

  const out = new NextResponse(upstream.body, { status: upstream.status });
  for (const h of ["content-type", "content-disposition", "x-content-type-options"]) {
    const v = upstream.headers.get(h);
    if (v) out.headers.set(h, v);
  }
  if (upstream.status === 401) out.cookies.delete(SESSION_COOKIE);
  return out;
}

export const GET = forward, POST = forward, PUT = forward, DELETE = forward;
export const dynamic = "force-dynamic";
