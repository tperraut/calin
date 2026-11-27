import { describe, expect, it } from "vite-plus/test";
import manifestText from "../../public/manifest.webmanifest?raw";
import indexHtml from "../../index.html?raw";

// `public/` is copied as it is to the root of the build, so these are the files the build serves.
const publicPngs = import.meta.glob<string>("../../public/**/*.png", {
  query: "?inline",
  import: "default",
  eager: true,
});

/** Width and height from a PNG's header (its IHDR chunk). */
function pngSize(dataUrl: string): { width: number; height: number } {
  const bytes = Uint8Array.from(atob(dataUrl.slice(dataUrl.indexOf(",") + 1)), (char) =>
    char.charCodeAt(0),
  );
  const signature = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  expect(bytes.slice(0, 8)).toEqual(signature);
  expect(String.fromCharCode(...bytes.slice(12, 16))).toBe("IHDR");
  const view = new DataView(bytes.buffer);
  return { width: view.getUint32(16), height: view.getUint32(20) };
}

function publicFile(src: string): string {
  const data = publicPngs[`../../public${src}`];
  if (data === undefined) throw new Error(`${src} is not in public/`);
  return data;
}

type Icon = { src: string; sizes: string; type: string; purpose?: string };

describe("the web app manifest", () => {
  const manifest = JSON.parse(manifestText) as Record<string, unknown>;

  it("has what makes the app installable", () => {
    expect(manifest).toMatchObject({
      name: "Calin",
      short_name: "Calin",
      display: "standalone",
      start_url: "/",
      scope: "/",
    });
    // The light theme's --bg (index.css); index.html sets both themes for the browser's chrome.
    expect(manifest.theme_color).toBe("#f7f7f5");
    expect(manifest.background_color).toBe("#f7f7f5");
  });

  it("names icons at 192 and 512 px and a maskable one, each present at its stated size", () => {
    const icons = manifest.icons as Icon[];
    const sizes = icons.map((icon) => `${icon.sizes} ${icon.purpose ?? "any"}`);
    expect(sizes).toEqual(
      expect.arrayContaining(["192x192 any", "512x512 any", "512x512 maskable"]),
    );
    for (const icon of icons) {
      expect(icon.type).toBe("image/png");
      const [width, height] = icon.sizes.split("x").map(Number);
      expect(pngSize(publicFile(icon.src))).toEqual({ width, height });
    }
  });

  it("is linked from the page, with what iOS reads instead", () => {
    expect(indexHtml).toContain('<link rel="manifest" href="/manifest.webmanifest" />');
    expect(indexHtml).toContain('<meta name="apple-mobile-web-app-capable" content="yes" />');
    expect(indexHtml).toMatch(
      /name="theme-color" content="#f7f7f5" media="\(prefers-color-scheme: light\)"/,
    );
    expect(indexHtml).toMatch(
      /name="theme-color" content="#141513" media="\(prefers-color-scheme: dark\)"/,
    );
    const touchIcon = /<link rel="apple-touch-icon" href="([^"]+)"/.exec(indexHtml)?.[1] ?? "";
    expect(pngSize(publicFile(touchIcon))).toEqual({ width: 180, height: 180 });
  });

  it("has the badge a notification shows in the status bar", () => {
    expect(pngSize(publicFile("/icons/badge-96.png"))).toEqual({ width: 96, height: 96 });
  });
});
