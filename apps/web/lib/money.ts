// Money arrives from the API as exact decimal strings ("5200.0000"). It is NEVER parsed into a JS
// number: display formatting works on the string so no binary floating point touches an amount.
export function formatMoney(value: string | null | undefined): string {
  if (value === null || value === undefined || value === "") return "—";
  const m = /^(-?)(\d+)(?:\.(\d+))?$/.exec(value.trim());
  if (!m) return value;
  const [, sign, int, frac = ""] = m;
  const grouped = int.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  const cents = (frac + "00").slice(0, 2);
  // Show sub-cent precision only when it is non-zero (e.g. a $0.125/lb rate).
  const extra = frac.slice(2).replace(/0+$/, "");
  return `${sign}$${grouped}.${cents}${extra}`;
}

/** Validate a user-typed decimal for sending as a string (max 4 places, no exponent). */
export function isValidDecimal(s: string): boolean {
  return /^-?\d{1,10}(\.\d{1,4})?$/.test(s.trim());
}
