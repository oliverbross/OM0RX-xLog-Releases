/** MHz, kHz, then tens of Hz. Keep the original integer Hz for storage and control. */
export function formatRadioFrequency(hz: number): string {
  const digits = String(Math.floor(hz / 10)).padStart(6, "0");
  return `${digits.slice(0, -5)}.${digits.slice(-5, -2)}.${digits.slice(-2)}`;
}

export function decimalMHz(input: string): string {
  return input.trim().replace(/^(\d+)\.(\d{3})\.(\d{1,2})$/, "$1.$2$3");
}
