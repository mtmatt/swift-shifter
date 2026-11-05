// The bar under the main window telling of a new version: a button installs
// it where the app can update itself, and opens its release page elsewhere.

/** A new version, as `check_update` gives it. */
export interface UpdateInfo {
  version: string;
  can_install: boolean;
  release_url: string;
}

/** Where the bar's text and button go; a null action hides the button. */
export type ShowUpdate = (text: string, action: string | null) => void;

export interface UpdateActions {
  install(): Promise<void>;
  open(url: string): Promise<void>;
  /** Calls `progress` with the download's percentage until it's unlistened. */
  onProgress(progress: (percent: number) => void): Promise<() => void>;
}

export function offerUpdate(info: UpdateInfo, show: ShowUpdate): void {
  show(`v${info.version} available`, info.can_install ? "install" : "download");
}

/**
 * What the bar's button does for `info`. An install that works restarts the
 * app, so what's shown after one is what went wrong, with the button to try
 * again.
 */
export async function actOnUpdate(
  info: UpdateInfo,
  show: ShowUpdate,
  actions: UpdateActions,
): Promise<void> {
  try {
    if (!info.can_install) {
      await actions.open(info.release_url);
      return;
    }
    show("installing… 0%", null);
    const unlisten = await actions.onProgress((percent) =>
      show(`installing… ${Math.round(percent)}%`, null),
    );
    try {
      await actions.install();
    } finally {
      unlisten();
    }
  } catch (err) {
    show(`update failed: ${String(err)}`, "retry");
  }
}
