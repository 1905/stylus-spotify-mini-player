// Settings → Updates: the status line and its button, from Rust updater.rs (update_status / update-status).

// Check now is offered only when nothing is on its way: no check, no download, nothing staged
const CHECKABLE = new Set(["idle", "upToDate", "available", "failed"]);

/**
 * The Updates row for an update_status payload ({status, version, message, current, auto}).
 * text: the status line ("" = none); check: show Check now; restart: show Restart now.
 * null or an unknown status (no updater in this build) = an empty row, no buttons.
 */
export function updateView(s) {
  const status = s && s.status;
  const v = (s && s.version) || "";
  const cur = (s && s.current) || "";
  const text = {
    idle: cur ? `Version ${cur}` : "",
    checking: "Checking for updates…",
    upToDate: cur ? `Up to date (${cur})` : "Up to date",
    available: v ? `${v} is available` : "An update is available",
    downloading: v ? `Downloading ${v}…` : "Downloading the update…",
    staged: v ? `${v} installs when Stylus quits` : "The update installs when Stylus quits",
    failed: s && s.message ? `Update failed: ${s.message}` : "Update failed",
    notInstallable: "Updates need Stylus in Applications",
  }[status];
  if (text === undefined) return { text: "", check: false, restart: false };
  return { text, check: CHECKABLE.has(status), restart: status === "staged" };
}
