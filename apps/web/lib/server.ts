// Server-only helpers for the BFF route handlers.
export const SESSION_COOKIE = "xflow_session";

export function backendUrl(): string {
  return process.env.API_INTERNAL_URL ?? "http://127.0.0.1:8080";
}

export function cookieOptions(maxAgeSeconds: number) {
  return {
    httpOnly: true,
    sameSite: "lax" as const,
    secure: process.env.NODE_ENV === "production" && process.env.INSECURE_COOKIES !== "1",
    path: "/",
    maxAge: maxAgeSeconds,
  };
}

/**
 * CSRF defense for the cookie-authenticated BFF. Browsers always send `Sec-Fetch-Site` (and cannot be
 * made to forge it), so it is authoritative when present; some browsers omit `Origin` on same-origin POSTs.
 * Order: a mismatching Origin always fails; then Sec-Fetch-Site; then Referer; with no browser signals at
 * all, only safe (GET/HEAD) requests pass. Non-browser clients should call the API with a Bearer token.
 */
export function sameOrigin(req: Request): boolean {
  const safe = req.method === "GET" || req.method === "HEAD";
  const host = req.headers.get("x-forwarded-host") ?? req.headers.get("host");
  const matchesHost = (url: string) => { try { return new URL(url).host === host; } catch { return false; } };

  const origin = req.headers.get("origin");
  if (origin && !matchesHost(origin)) return false;

  const site = req.headers.get("sec-fetch-site");
  if (site) return site === "same-origin" || (safe && site === "none");

  if (origin) return true; // matched above
  const referer = req.headers.get("referer");
  if (referer) return matchesHost(referer);
  return safe;
}
