import { describe, expect, it } from "vitest";
import { matchScore, parseQuery, rankFiles, relativeTime, type RecentFile } from "./quick-search";

function file(name: string, modified_ms: number): RecentFile {
  return { path: `/tmp/${name}`, name, folder: "/tmp", modified_ms };
}

describe("parseQuery", () => {
  it("returns plain text when there is no target format", () => {
    expect(parseQuery("  holiday photo ")).toEqual({ text: "holiday photo", format: null });
  });

  it("splits off a trailing 'to <format>'", () => {
    expect(parseQuery("IMG_2041 to JPG")).toEqual({ text: "IMG_2041", format: "jpg" });
  });

  it("accepts a leading dot on the format", () => {
    expect(parseQuery("report to .pdf")).toEqual({ text: "report", format: "pdf" });
  });

  it("keeps 'to' that is part of the file name", () => {
    expect(parseQuery("trip to tokyo.png")).toEqual({ text: "trip to tokyo.png", format: null });
  });
});

describe("matchScore", () => {
  it("is null unless every word appears", () => {
    expect(matchScore("holiday-photo.png", "photo beach")).toBeNull();
  });

  it("ranks a match at a word start above one mid-word", () => {
    const atStart = matchScore("holiday-photo.png", "photo")!;
    const midWord = matchScore("telephoto.png", "photo")!;
    expect(atStart).toBeGreaterThan(midWord);
  });

  it("is case-insensitive", () => {
    expect(matchScore("Report.PDF", "report pdf")).not.toBeNull();
  });
});

describe("rankFiles", () => {
  const files = [file("old.png", 100), file("newest.jpg", 300), file("middle.md", 200)];

  it("lists the newest files for an empty query", () => {
    expect(rankFiles(files, "", 2).map((f) => f.name)).toEqual(["newest.jpg", "middle.md"]);
  });

  it("puts better matches first and breaks ties by age", () => {
    const candidates = [
      file("telephoto.png", 300),
      file("photo-a.png", 100),
      file("photo-b.png", 200),
      file("unrelated.txt", 400),
    ];
    expect(rankFiles(candidates, "photo", 10).map((f) => f.name)).toEqual([
      "photo-b.png",
      "photo-a.png",
      "telephoto.png",
    ]);
  });
});

describe("relativeTime", () => {
  const now = Date.UTC(2026, 9, 15, 12, 0, 0);

  it("buckets recent times by unit", () => {
    expect(relativeTime(now - 30_000, now)).toBe("now");
    expect(relativeTime(now - 5 * 60_000, now)).toBe("5m");
    expect(relativeTime(now - 3 * 3_600_000, now)).toBe("3h");
    expect(relativeTime(now - 2 * 86_400_000, now)).toBe("2d");
  });

  it("falls back to a date after a week", () => {
    expect(relativeTime(now - 10 * 86_400_000, now)).not.toMatch(/^\d+[mhd]$/);
  });
});
