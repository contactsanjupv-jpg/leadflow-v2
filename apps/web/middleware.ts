import { NextResponse, type NextRequest } from "next/server";
import { gateRedirect } from "./lib/session-redirect";

// Gate app pages on the presence of a session cookie. This is UX only: the API validates the
// session on every request, so a forged or expired cookie gets a 401 in app/(app)/layout.tsx and the
// user is sent to /login?expired=1 (which this gate deliberately lets through; see lib/session-redirect.ts).
export function middleware(req: NextRequest) {
  const has = req.cookies.get("xflow_session")?.value;
  const target = gateRedirect(Boolean(has), req.nextUrl.pathname, req.nextUrl.searchParams);
  return target ? NextResponse.redirect(new URL(target, req.url)) : NextResponse.next();
}

export const config = { matcher: ["/((?!api|_next/static|_next/image|favicon.ico).*)"] };
