import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createLoader } from "./latest-load";

/** A fetcher whose responses the test resolves or rejects by hand, to control arrival order. */
function controlled() {
  const pending: Record<string, { resolve: (v: string[]) => void; reject: (e: Error) => void }[]> = {};
  const fetcher = (path: string) => new Promise<string[]>((resolve, reject) => { (pending[path] ??= []).push({ resolve, reject }); });
  return { fetcher, pending };
}
function harness() {
  const c = controlled();
  const state = { data: null as string[] | null, error: null as string | null, loading: true, dataWrites: 0 };
  const loader = createLoader<string[]>(c.fetcher, {
    data: (d) => { state.data = d; state.dataWrites++; },
    error: (m) => { state.error = m; },
    loading: (b) => { state.loading = b; },
  });
  return { ...c, state, loader };
}
const tick = () => new Promise((r) => setImmediate(r));

test("REGRESSION (e2e review queue): an older response arriving last must not overwrite the newer list", async () => {
  const h = harness();
  // After a review action the page reloads the NEW list; the user immediately switches the filter to CONFIRMED.
  const reloadNew = h.loader.load("/discrepancies?status=NEW");
  const loadConfirmed = h.loader.load("/discrepancies?status=CONFIRMED");
  h.pending["/discrepancies?status=CONFIRMED"][0].resolve(["confirmed row"]);   // the newer request answers first
  await loadConfirmed;
  assert.deepEqual(h.state.data, ["confirmed row"]);
  h.pending["/discrepancies?status=NEW"][0].resolve(["new row"]);                // the stale one answers last
  await reloadNew;
  assert.deepEqual(h.state.data, ["confirmed row"], "the table must keep showing the CONFIRMED rows");
  assert.equal(h.state.dataWrites, 1, "the stale response is never applied");
});

test("in-order responses still apply, and each reload shows fresh data", async () => {
  const h = harness();
  const a = h.loader.load("/x"); h.pending["/x"][0].resolve(["one"]); await a;
  assert.deepEqual(h.state.data, ["one"]);
  const b = h.loader.load("/x"); h.pending["/x"][1].resolve(["two"]); await b;
  assert.deepEqual(h.state.data, ["two"]);
  assert.equal(h.state.loading, false); assert.equal(h.state.error, null);
});

test("a stale request that fails does not set an error over newer data", async () => {
  const h = harness();
  const stale = h.loader.load("/old");
  const fresh = h.loader.load("/new");
  h.pending["/new"][0].resolve(["fresh"]); await fresh;
  h.pending["/old"][0].reject(new Error("boom")); await stale;
  assert.deepEqual(h.state.data, ["fresh"]);
  assert.equal(h.state.error, null);
});

test("the spinner stays on until the NEWEST load finishes", async () => {
  const h = harness();
  const a = h.loader.load("/a"); const b = h.loader.load("/b");
  h.pending["/a"][0].resolve(["a"]); await a;
  assert.equal(h.state.loading, true, "an older load finishing must not hide the newer one in flight");
  h.pending["/b"][0].resolve(["b"]); await b;
  assert.equal(h.state.loading, false);
  assert.deepEqual(h.state.data, ["b"]);
});

test("a current failure is reported, and the next successful load clears it", async () => {
  const h = harness();
  const a = h.loader.load("/a"); h.pending["/a"][0].reject(new Error("API down")); await a;
  assert.equal(h.state.error, "API down"); assert.equal(h.state.loading, false);
  const b = h.loader.load("/a"); h.pending["/a"][1].resolve(["ok"]); await b;
  assert.equal(h.state.error, null); assert.deepEqual(h.state.data, ["ok"]);
});

test("cancel() (effect cleanup or unmount) stops a response in flight from updating state", async () => {
  const h = harness();
  const a = h.loader.load("/a");
  h.loader.cancel();
  h.pending["/a"][0].resolve(["late"]); await a; await tick();
  assert.equal(h.state.data, null);
  assert.equal(h.state.dataWrites, 0);
  const b = h.loader.load("/b"); h.pending["/b"][0].resolve(["after"]); await b;   // a later load works normally
  assert.deepEqual(h.state.data, ["after"]);
});

test("useLoad uses the guarded loader and cancels it on cleanup", () => {
  const src = readFileSync(new URL("../components/ui.tsx", import.meta.url), "utf8");
  assert.ok(src.includes('import { createLoader } from "@/lib/latest-load"'));
  const hook = src.slice(src.indexOf("export function useLoad"), src.indexOf("/** Run an action"));
  assert.ok(hook.includes("createLoader<T>") && hook.includes("loader.load(path)") && hook.includes("return () => loader.cancel()"));
  assert.ok(!hook.includes("setData(await api"), "the unguarded fetch-then-set pattern must not come back");
});

test("scripts/e2e.sh launches the standalone server, not `next start`, and keeps redirects on the request origin", () => {
  const sh = readFileSync(new URL("../../../scripts/e2e.sh", import.meta.url), "utf8");
  assert.ok(!/next start/.test(sh.replace(/#.*$/gm, "")), "the app builds with output: standalone, so next start is wrong");
  assert.ok(sh.includes("exec node server.js") && sh.includes(".next/standalone") && sh.includes(".next/static"));
  assert.ok(!/HOSTNAME=/.test(sh.replace(/#.*$/gm, "")), "HOSTNAME=127.0.0.1 makes redirects point at localhost, a different cookie origin");
});
