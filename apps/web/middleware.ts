import { NextResponse, type NextRequest } from "next/server";

// Gate app pages on the presence of a session cookie. This is UX only: the API validates the
// session on every request, so a forged cookie gets a 401 and is bounced back to /login.
export function middleware(req: NextRequest) {
  const has = req.cookies.get("xflow_session")?.value;
  const { pathname } = req.nextUrl;
  if (!has && pathname !== "/login") return NextResponse.redirect(new URL("/login", req.url));
  if (has && pathname === "/login") return NextResponse.redirect(new URL("/", req.url));
  return NextResponse.next();
}

export const config = { matcher: ["/((?!api|_next/static|_next/image|favicon.ico).*)"] };
