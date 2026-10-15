export const MIN_DATE = "1900-01-01";
export const MAX_DATE = "2100-12-31";

export function isPlausibleIsoDate(value: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    return false;
  }

  if (value < MIN_DATE || value > MAX_DATE) {
    return false;
  }

  const date = new Date(`${value}T00:00:00Z`);

  return (
    !Number.isNaN(date.getTime()) &&
    date.toISOString().slice(0, 10) === value
  );
}
