import { describe, expect, it } from "vite-plus/test";
import { describeUserAgent, signInUrl, tokenNameProblem } from "./auth";

const at = (pathname: string, search = "") => signInUrl({ pathname, search });

describe("signInUrl", () => {
  it("comes back to the current path and query", () => {
    expect(at("/")).toBe("/auth/login?next=%2F");
    expect(at("/settings")).toBe("/auth/login?next=%2Fsettings");
    expect(at("/s/abc/changes")).toBe("/auth/login?next=%2Fs%2Fabc%2Fchanges");
    expect(at("/s/abc/files", "?path=src%2Fa.ts&x=1")).toBe(
      "/auth/login?next=%2Fs%2Fabc%2Ffiles%3Fpath%3Dsrc%252Fa.ts%26x%3D1",
    );
  });

  it("never names another origin", () => {
    expect(at("//evil.example/steal")).toBe("/auth/login?next=%2F");
    expect(at("/\\evil.example")).toBe("/auth/login?next=%2F");
    expect(at("https://evil.example/")).toBe("/auth/login?next=%2F");
    // A query is only ever part of the encoded `next`, never a parameter of its own.
    expect(at("/", "?next=https://evil.example")).toBe(
      "/auth/login?next=%2F%3Fnext%3Dhttps%3A%2F%2Fevil.example",
    );
  });
});

describe("describeUserAgent", () => {
  it("names common browsers and systems", () => {
    expect(
      describeUserAgent("Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0"),
    ).toBe("Firefox on Linux");
    expect(
      describeUserAgent(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
      ),
    ).toBe("Chrome on Windows");
    expect(
      describeUserAgent(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0",
      ),
    ).toBe("Edge on Windows");
    expect(
      describeUserAgent(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1",
      ),
    ).toBe("Safari on iOS");
    expect(
      describeUserAgent(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
      ),
    ).toBe("Safari on macOS");
    expect(
      describeUserAgent(
        "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36",
      ),
    ).toBe("Chrome on Android");
  });

  it("shows anything else as it is, cut short", () => {
    expect(describeUserAgent("curl/8.9.1")).toBe("curl/8.9.1");
    const long = `SomeClient/1.0 ${"x".repeat(100)}`;
    const shown = describeUserAgent(long);
    expect(shown).toHaveLength(60);
    expect(shown.endsWith("…")).toBe(true);
    expect(describeUserAgent(null)).toBe("Unknown browser");
    expect(describeUserAgent("  ")).toBe("Unknown browser");
  });
});

describe("tokenNameProblem", () => {
  it("wants 1 to 80 characters", () => {
    expect(tokenNameProblem("")).toBe("Give the token a name.");
    expect(tokenNameProblem("   ")).toBe("Give the token a name.");
    expect(tokenNameProblem("a")).toBeUndefined();
    expect(tokenNameProblem("é".repeat(80))).toBeUndefined();
    expect(tokenNameProblem("a".repeat(81))).toBe("Keep the name to 80 characters.");
  });
});
