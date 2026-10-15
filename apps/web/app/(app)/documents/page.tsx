"use client";
import { useState } from "react";
import { api } from "@/lib/api";
import { Badge, Card, Err, useAction, useLoad } from "@/components/ui";

const TYPES = ["INVOICE", "CONTRACT", "RATE_SHEET", "MANIFEST", "WEIGHT_TICKET", "OTHER"];

function Detail({ id, status, onChanged }: { id: string; status: string; onChanged: () => void }) {
  const d = useLoad(`/documents/${id}`);
  const act = useAction();
  const [note, setNote] = useState("");
  const text: string | undefined = d.data?.extraction?.raw_extraction?.text;
  const needsWork = status === "REVIEW_REQUIRED" || status === "FAILED";
  return (
    <div>
      <Err e={d.error} />
      {needsWork && (
        <p>{text ? "Text was extracted from this file. Check it and enter the data by hand — nothing is created automatically." : "This file could not be read automatically (scans and photos are not OCR'd), so enter its data by hand."}{" "}
          Link the document when you save the invoice, manifest, weight ticket or contract (<i>Source document</i> field) and it is marked processed automatically.</p>
      )}
      {text && <details open><summary>Extracted text ({d.data.extraction.extractor_provider})</summary><pre data-testid="extracted-text">{text}</pre></details>}
      {needsWork && (
        <div className="row">
          <label>Note (optional)<input value={note} onChange={(e) => setNote(e.target.value)} size={36} /></label>
          <button className="sec" disabled={act.busy} onClick={async () => { if (await act.run(() => api("POST", `/documents/${id}/resolve`, { note: note || undefined }))) onChanged(); }}>Mark as manually processed</button>
        </div>
      )}
      <Err e={act.error} />
    </div>
  );
}

export default function Documents() {
  const list = useLoad("/documents?limit=100");
  const [type, setType] = useState("INVOICE");
  const [file, setFile] = useState<File | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const act = useAction();

  async function upload(e: React.FormEvent) {
    e.preventDefault();
    if (!file) return;
    const form = e.target as HTMLFormElement;
    const r = await act.run(() => api("POST", `/documents?document_type=${type}&filename=${encodeURIComponent(file.name)}`, undefined, { raw: file, contentType: file.type || "application/octet-stream" }), "Uploaded");
    if (r) { setFile(null); form.reset(); setOpen(r.id); await list.reload(); }
  }
  return (
    <>
      <h1>Documents</h1>
      <Card title="Upload">
        <p className="muted">PDF, PNG, JPEG, TIFF, text or CSV, up to 25 MB. Text-based PDFs and text files are read automatically for you to check; scans and photos are stored as evidence and you enter their data by hand. Nothing extracted is ever turned into financial records automatically.</p>
        <form className="row" onSubmit={upload}>
          <label>Type<select value={type} onChange={(e) => setType(e.target.value)}>{TYPES.map((t) => <option key={t}>{t}</option>)}</select></label>
          <label>File<input type="file" data-testid="document-file" onChange={(e) => setFile(e.target.files?.[0] ?? null)} required /></label>
          <button disabled={act.busy || !file}>Upload</button>
        </form>
        <Err e={act.error} />{act.ok && <div className="okmsg">{act.ok}</div>}
      </Card>
      <Card title="Uploaded documents">
        <Err e={list.error} />
        <table><thead><tr><th>File</th><th>Type</th><th>Status</th><th>Detail</th><th>Uploaded</th></tr></thead>
          <tbody>{(list.data ?? []).flatMap((d: any) => [
            <tr key={d.id} style={{ cursor: "pointer" }} onClick={() => setOpen(open === d.id ? null : d.id)} data-testid={`doc-row`}>
              <td>{d.original_filename}</td><td>{d.document_type}</td><td><Badge s={d.status} /></td><td className="muted">{d.status_detail}</td><td>{new Date(d.created_at).toLocaleString()}</td></tr>,
            open === d.id && <tr key={d.id + "-x"}><td colSpan={5}><a href={`/api/proxy/documents/${d.id}/download`}>Download original</a><Detail id={d.id} status={d.status} onChanged={list.reload} /></td></tr>,
          ])}
            {list.data?.length === 0 && <tr><td colSpan={5} className="muted">No documents uploaded.</td></tr>}</tbody></table>
      </Card>
    </>
  );
}
