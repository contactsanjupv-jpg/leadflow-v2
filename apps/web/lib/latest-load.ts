// Latest-request-wins loader behind useLoad. Without it two overlapping GETs (for example the reload that
// follows a review action and the reload that follows a status-filter change) can resolve out of order, and
// the OLDER response then overwrites the newer one: the table shows the previous filter's rows under the
// new filter, and the row a user (or a test) is about to click is replaced mid-click.
import { createLatestGuard } from "./evidence";

export interface LoaderSinks<T> {
  data(d: T): void;
  error(message: string | null): void;
  loading(isLoading: boolean): void;
}

export function createLoader<T>(fetcher: (path: string) => Promise<T>, sinks: LoaderSinks<T>) {
  const guard = createLatestGuard();
  const KEY = "load"; // one list per hook: any newer load (same or different path) supersedes older ones
  return {
    async load(path: string): Promise<void> {
      const token = guard.begin(KEY);
      sinks.loading(true);
      try {
        const d = await fetcher(path);
        if (!guard.isCurrent(KEY, token)) return; // a newer load started: this response is stale
        sinks.data(d);
        sinks.error(null);
      } catch (e: unknown) {
        if (!guard.isCurrent(KEY, token)) return;
        sinks.error((e as { message?: string })?.message ?? "Failed to load");
      } finally {
        // Only the newest load clears the spinner, so an older one finishing cannot hide a newer one in flight.
        if (guard.isCurrent(KEY, token)) sinks.loading(false);
      }
    },
    /** Effect cleanup (path change / unmount): nothing already in flight may update state afterwards. */
    cancel(): void { guard.invalidate(KEY); },
  };
}
