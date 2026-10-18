// Routing rules for the cookie-presence gate in middleware.ts. This is UX only: the API validates the
// session on every request, and app/(app)/layout.tsx rejects sessions the API refuses.
//
// Loop this module exists to prevent: a stale/expired/revoked `xflow_session` cookie is still *present*.
// The layout sends such a user to the login page, and the gate used to send anyone with a cookie away
// from the login page, so the browser bounced between "/" and "/login" forever. The layout now
// redirects to SESSION_EXPIRED_PATH and the gate lets that exact marker through.

export const LOGIN_PATH = "/login";
export const SESSION_EXPIRED_PARAM = "expired";
/** Where the app layout sends a user whose session cookie the API rejected (or could not verify). */
export const SESSION_EXPIRED_PATH = `${LOGIN_PATH}?${SESSION_EXPIRED_PARAM}=1`;

/** Path the browser should be redirected to, or null to let the request through. */
export function gateRedirect(hasSessionCookie: boolean, pathname: string, search: URLSearchParams): string | null {
  // No cookie: everything except the login page needs one.
  if (!hasSessionCookie && pathname !== LOGIN_PATH) return LOGIN_PATH;
  // Cookie present on the login page: skip the form and go to the app, UNLESS the app just told us
  // the cookie is bad. Redirecting then would send the user straight back and loop.
  if (hasSessionCookie && pathname === LOGIN_PATH && !search.has(SESSION_EXPIRED_PARAM)) return "/";
  return null;
}
