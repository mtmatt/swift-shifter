import { describe, expect, it, vi } from "vitest";
import { actOnUpdate, offerUpdate, type UpdateActions, type UpdateInfo } from "./update";

const installable: UpdateInfo = {
  version: "2.0.0",
  can_install: true,
  release_url: "https://example.com/v2.0.0",
};
const downloadable: UpdateInfo = { ...installable, can_install: false };

function actions(overrides: Partial<UpdateActions> = {}): UpdateActions {
  return {
    install: vi.fn(async () => {}),
    open: vi.fn(async () => {}),
    onProgress: vi.fn(async () => () => {}),
    ...overrides,
  };
}

describe("offerUpdate", () => {
  it("offers to install only what the app can install", () => {
    const show = vi.fn();
    offerUpdate(installable, show);
    offerUpdate(downloadable, show);
    expect(show.mock.calls).toEqual([
      ["v2.0.0 available", "install"],
      ["v2.0.0 available", "download"],
    ]);
  });
});

describe("actOnUpdate", () => {
  it("opens the release page where the app can't install, not installing", async () => {
    const act = actions();
    await actOnUpdate(downloadable, vi.fn(), act);
    expect(act.open).toHaveBeenCalledWith(downloadable.release_url);
    expect(act.install).not.toHaveBeenCalled();
  });

  it("shows the download's progress, with no button, and stops listening", async () => {
    const show = vi.fn();
    const unlisten = vi.fn();
    let progress = (_percent: number) => {};
    const act = actions({
      onProgress: async (listener) => {
        progress = listener;
        return unlisten;
      },
      install: async () => progress(41.6),
    });
    await actOnUpdate(installable, show, act);
    expect(show.mock.calls).toEqual([
      ["installing… 0%", null],
      ["installing… 42%", null],
    ]);
    expect(unlisten).toHaveBeenCalled();
  });

  it("says what went wrong and offers to try again", async () => {
    const show = vi.fn();
    const unlisten = vi.fn();
    const act = actions({
      onProgress: async () => unlisten,
      install: async () => {
        throw "Wait for the conversions to finish";
      },
    });
    await actOnUpdate(installable, show, act);
    expect(show).toHaveBeenLastCalledWith(
      "update failed: Wait for the conversions to finish",
      "retry",
    );
    expect(unlisten).toHaveBeenCalled();
  });

  it("says so when the release page can't open", async () => {
    const show = vi.fn();
    const act = actions({
      open: async () => {
        throw "no browser";
      },
    });
    await actOnUpdate(downloadable, show, act);
    expect(show).toHaveBeenLastCalledWith("update failed: no browser", "retry");
  });
});
