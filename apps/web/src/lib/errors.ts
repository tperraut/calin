/** A message fit for the UI from whatever a request threw. */
export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
