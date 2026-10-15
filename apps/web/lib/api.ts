"use client";
// Browser -> Next.js BFF (/api/proxy/*). The session token lives in an httpOnly cookie and is
// attached server-side; client code never sees it.

export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string) {
    super(message);
  }
}

export async function api<T = any>(method: string, path: string, body?: unknown, init?: { raw?: BodyInit; contentType?: string }): Promise<T> {
  const headers: Record<string, string> = {};
  let payload: BodyInit | undefined;
  if (init?.raw !== undefined) {
    payload = init.raw;
    headers["content-type"] = init.contentType ?? "application/octet-stream";
  } else if (body !== undefined) {
    payload = JSON.stringify(body);
    headers["content-type"] = "application/json";
  }
  const res = await fetch(`/api/proxy${path}`, { method, headers, body: payload, credentials: "same-origin" });
  if (res.status === 401 && typeof window !== "undefined") {
    window.location.href = "/login";
    throw new ApiError(401, "AUTHENTICATION_ERROR", "Session expired");
  }
  const text = await res.text();
  let data: any = null;
  try { data = text ? JSON.parse(text) : null; } catch { /* non-JSON */ }
  if (!res.ok) {
    throw new ApiError(res.status, data?.error?.code ?? "ERROR", data?.error?.message ?? `Request failed (${res.status})`);
  }
  return data as T;
}
