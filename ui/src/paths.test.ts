import { describe, expect, it } from "vitest";
import { folderLabel } from "./paths";

describe("folderLabel", () => {
  it("splits a POSIX path into the folder name and its parent", () => {
    expect(folderLabel("/Users/me/Clients/acme")).toEqual({ name: "acme", parent: "/Users/me/Clients" });
  });

  it("ignores a trailing separator", () => {
    expect(folderLabel("/Users/me/Downloads/")).toEqual({ name: "Downloads", parent: "/Users/me" });
  });

  it("handles Windows paths", () => {
    expect(folderLabel("C:\\Users\\me\\Documents")).toEqual({ name: "Documents", parent: "C:\\Users\\me" });
  });

  it("keeps the root as the parent of a top-level folder", () => {
    expect(folderLabel("/data")).toEqual({ name: "data", parent: "/" });
    expect(folderLabel("C:\\Users")).toEqual({ name: "Users", parent: "C:\\" });
  });

  it("has no parent for a bare name", () => {
    expect(folderLabel("Documents")).toEqual({ name: "Documents", parent: "" });
  });

  it("shows a root whole", () => {
    expect(folderLabel("/")).toEqual({ name: "/", parent: "" });
    expect(folderLabel("C:\\")).toEqual({ name: "C:\\", parent: "" });
  });
});
