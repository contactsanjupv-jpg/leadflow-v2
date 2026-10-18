import { cookies } from "next/headers";
import { redirect } from "next/navigation";
import type { ReactNode } from "react";
import Nav from "@/components/Nav";
import { backendUrl, SESSION_COOKIE } from "@/lib/server";
import { SESSION_EXPIRED_PATH } from "@/lib/session-redirect";

export const dynamic = "force-dynamic";

export default async function AppLayout({ children }: { children: ReactNode }) {
  const token = cookies().get(SESSION_COOKIE)?.value;
  if (!token) redirect("/login");
  const res = await fetch(`${backendUrl()}/api/v1/me`, { headers: { authorization: `Bearer ${token}` }, cache: "no-store" }).catch(() => null);
  // Not plain "/login": the cookie is still present, and the middleware would bounce us back to "/".
  if (!res || res.status === 401) redirect(SESSION_EXPIRED_PATH);
  if (!res.ok) return <main><div className="card err">The API is unavailable. Try again shortly.</div></main>;
  const me = await res.json();
  return (
    <>
      <Nav email={me.email} org={me.organization_name} role={me.role} />
      <main>
        {!me.entitled && (
          <div className="card" style={{ borderColor: "#b54708" }} data-testid="no-subscription-banner">
            No active subscription: reading your data works, but running reconciliations and monitoring are disabled until billing is active. <a href="/settings">Subscribe in Settings</a>
          </div>
        )}
        {children}
      </main>
    </>
  );
}
