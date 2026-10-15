"use client";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";

const links = [
  ["/", "Dashboard"], ["/invoices", "Invoices"], ["/review", "Review queue"], ["/disputes", "Disputes & recovery"],
  ["/data", "Contracts & data"], ["/documents", "Documents"], ["/settings", "Settings"],
];

export default function Nav({ email, org, role }: { email: string; org: string; role: string }) {
  const path = usePathname();
  const router = useRouter();
  async function logout() {
    await fetch("/api/auth/logout", { method: "POST" });
    router.push("/login");
    router.refresh();
  }
  return (
    <nav className="top">
      <span className="brand">XFlow</span>
      {links.map(([href, label]) => (
        <Link key={href} href={href} className={(href === "/" ? path === "/" : path.startsWith(href)) ? "active" : ""}>{label}</Link>
      ))}
      <span className="sp" />
      <span title={email}>{org} · {role}</span>
      <button className="sec" onClick={logout}>Log out</button>
    </nav>
  );
}
