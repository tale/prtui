import { readFileSync } from "node:fs";
import { defineLoader } from "vitepress";

export interface Scheme {
  slug: string;
  name: string;
  author: string;
  variant: "dark" | "light";
  colors: Record<string, string>;
}

declare const data: Scheme[];
export { data };

const LUMA_MAX = 255_000;

export default defineLoader({
  watch: ["../../../crates/prtui-tui/schemes.conf"],
  load([path]) {
    return readFileSync(path, "utf8")
      .split("\n[")
      .slice(1)
      .map((section) => {
        const [slug, body] = section.split(/\]\n(.*)/s);
        return scheme(slug, body);
      });
  },
});

function scheme(slug: string, body: string): Scheme {
  const base = Array.from({ length: 16 }, (_, index) => {
    const slot = index.toString(16);
    const value = body.match(new RegExp(`^\\s*base0${slot}:\\s*"?#([0-9a-f]{6})`, "im"));
    return parseInt(value![1], 16);
  });
  const background = base[0x0];

  // Same blend strengths as the renderer, interpolated by background luma.
  const luma = channels(background).reduce(
    (sum, value, index) => sum + value * [299, 587, 114][index],
    0,
  );
  const tint = (hue: number, dark: number, light: number) => {
    const percent = Math.floor((dark * (LUMA_MAX - luma) + light * luma) / LUMA_MAX);
    return css(mix(background, hue, percent));
  };

  return {
    slug,
    name: field(body, "name") || slug,
    author: field(body, "author").replace(/\s*[(<][^)>]*[)>]/g, ""),
    variant: field(body, "variant") === "light" ? "light" : "dark",
    colors: {
      background: css(background),
      foreground: css(base[0x5]),
      comment: css(base[0x3]),
      muted: css(base[0x4]),
      red: css(base[0x8]),
      orange: css(base[0x9]),
      yellow: css(base[0xa]),
      green: css(base[0xb]),
      cyan: css(base[0xc]),
      blue: css(base[0xd]),
      magenta: css(base[0xe]),
      hunk: tint(base[0xd], 10, 12),
      add: tint(base[0xb], 26, 22),
      delete: tint(base[0x8], 18, 12),
      addEmphasis: tint(base[0xb], 45, 40),
      deleteEmphasis: tint(base[0x8], 35, 25),
    },
  };
}

function field(body: string, key: string) {
  return body.match(new RegExp(`^${key}:\\s*"?([^"\\n]*)`, "m"))?.[1].trim() ?? "";
}

function channels(color: number) {
  return [(color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff];
}

function mix(from: number, to: number, percent: number) {
  const [a, b] = [channels(from), channels(to)];
  return a.reduce(
    (color, value, index) =>
      (color << 8) | Math.floor((value * (100 - percent) + b[index] * percent) / 100),
    0,
  );
}

function css(color: number) {
  return `#${color.toString(16).padStart(6, "0")}`;
}
