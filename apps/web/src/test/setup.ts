import { afterEach, beforeEach, expect, vi } from "vite-plus/test";

// Solid's dev build reports reactivity mistakes (an untracked read, a write in
// an owned scope) through the console. A test that triggers one fails here,
// since no browser run would otherwise surface it.
let complaints: string[] = [];

beforeEach(() => {
  // What one test's composer remembered must not seed the next one's.
  localStorage.clear();
  complaints = [];
  const record = (...args: unknown[]): void => {
    complaints.push(args.map(String).join(" "));
  };
  vi.spyOn(console, "warn").mockImplementation(record);
  vi.spyOn(console, "error").mockImplementation(record);
});

afterEach(() => {
  vi.restoreAllMocks();
  expect(complaints).toEqual([]);
});
