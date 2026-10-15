"use client";
import { MAX_DATE, MIN_DATE } from "@/lib/dates";

/** The single date field used everywhere. `min`/`max` make the browser flag a partial year (e.g. "0206")
 *  as invalid and block form submission; the API and the database enforce the same range. */
export default function DateInput({ value, onChange, required }: { value: string; onChange: (v: string) => void; required?: boolean }) {
  return (
    <input
      type="date"
      value={value}
      min={MIN_DATE}
      max={MAX_DATE}
      required={required}
      title="Enter the full 4-digit year (1900-2100)"
      onChange={(e) => onChange(e.target.value)}
    />
  );
}
