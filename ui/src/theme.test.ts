import { describe, expect, it } from "vitest";
import { effectiveTheme } from "./theme";

describe("effectiveTheme", () => {
  it("uses the chosen theme whatever the system's", () => {
    for (const systemLight of [true, false]) {
      expect(effectiveTheme("light", systemLight)).toBe("light");
      expect(effectiveTheme("dark", systemLight)).toBe("dark");
    }
  });

  it("follows the system for system, a missing value or one it doesn't know", () => {
    for (const appearance of ["system", undefined, "sepia", 3]) {
      expect(effectiveTheme(appearance, true)).toBe("light");
      expect(effectiveTheme(appearance, false)).toBe("dark");
    }
  });
});
