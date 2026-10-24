import { describe, expect, it } from "vitest";
import { warningLine, warningTooltip } from "./warnings";

const rawHtml = { message: "2 raw HTML elements kept only their text", count: 2, line: null };
const loop = { message: "1 Typst loop wasn't run at line 4", count: 1, line: 4 };

describe("warningLine", () => {
  it("is null when nothing was left out", () => {
    expect(warningLine([])).toBeNull();
  });

  it("joins the backend's messages, lines and all", () => {
    expect(warningLine([rawHtml])).toBe("⚠ 2 raw HTML elements kept only their text");
    expect(warningLine([rawHtml, loop])).toBe(
      "⚠ 2 raw HTML elements kept only their text · 1 Typst loop wasn't run at line 4",
    );
  });
});

describe("warningTooltip", () => {
  it("puts each message on a line of its own", () => {
    expect(warningTooltip([rawHtml, loop])).toBe(
      "2 raw HTML elements kept only their text\n1 Typst loop wasn't run at line 4",
    );
  });
});
