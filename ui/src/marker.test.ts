import { describe, expect, it } from "vitest";
import { markerStatus } from "./marker";

describe("markerStatus", () => {
  it("says marker-pdf is installed", () => {
    expect(markerStatus(true, "Install it with: pipx install marker-pdf")).toEqual({
      text: "installed",
      className: "hint hint--ok",
    });
  });

  it("says how to install marker-pdf when it's missing", () => {
    const hint = "Install it with: pipx install marker-pdf";
    expect(markerStatus(false, hint)).toEqual({
      text: `not installed · ~2 GB download. ${hint}`,
      className: "hint hint--warn",
    });
  });

  it("still says it's missing without a hint", () => {
    expect(markerStatus(false, "").text).toBe("not installed · ~2 GB download.");
  });
});
