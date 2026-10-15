"use client";
import { useLoad } from "@/components/ui";

/** Optional "source document" picker. Documents awaiting manual entry are listed first; linking one when
 *  saving a record marks it PROCESSED on the server. */
export default function SourceDoc({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const docs = useLoad<any[]>("/documents?limit=200");
  const list = [...(docs.data ?? [])].sort((a, b) => Number(b.status === "REVIEW_REQUIRED") - Number(a.status === "REVIEW_REQUIRED"));
  return (
    <label>Source document
      <select value={value} onChange={(e) => onChange(e.target.value)} data-testid="source-document-select">
        <option value="">None</option>
        {list.map((d) => <option key={d.id} value={d.id}>{d.original_filename}{d.status === "REVIEW_REQUIRED" ? " (needs data entry)" : ""}</option>)}
      </select>
    </label>
  );
}
