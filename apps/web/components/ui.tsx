"use client";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { api } from "@/lib/api";
import { formatMoney } from "@/lib/money";

export function Money({ v }: { v: string | null | undefined }) {
  return <span className="num">{formatMoney(v)}</span>;
}

export function Badge({ s }: { s: string | null | undefined }) {
  if (!s) return <span className="muted">—</span>;
  return <span className={`badge b-${s}`}>{s.replaceAll("_", " ")}</span>;
}

export function Err({ e }: { e: string | null }) {
  return e ? <div className="err" role="alert">{e}</div> : null;
}

/** Load JSON from the API and expose reload + error state. */
export function useLoad<T = any>(path: string | null) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const reload = useCallback(async () => {
    if (!path) return;
    setLoading(true);
    try { setData(await api<T>("GET", path)); setError(null); }
    catch (e: any) { setError(e.message ?? "Failed to load"); }
    finally { setLoading(false); }
  }, [path]);
  useEffect(() => { void reload(); }, [reload]);
  return { data, error, loading, reload };
}

/** Run an action with busy/error/success state. */
export function useAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState<string | null>(null);
  const run = async <T,>(fn: () => Promise<T>, success?: string): Promise<T | undefined> => {
    setBusy(true); setError(null); setOk(null);
    try { const r = await fn(); if (success) setOk(success); return r; }
    catch (e: any) { setError(e.message ?? "Request failed"); return undefined; }
    finally { setBusy(false); }
  };
  return { busy, error, ok, run };
}

export function Card({ title, children }: { title?: string; children: ReactNode }) {
  return <section className="card">{title && <h2>{title}</h2>}{children}</section>;
}

export function Empty({ children }: { children: ReactNode }) {
  return <p className="muted">{children}</p>;
}
