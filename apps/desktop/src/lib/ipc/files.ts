// Native file selection for credential and invitation paths
// (runtime provisioning). The dialog plugin is a Tauri-only
// capability: the import stays lazy so browser/bun contexts (and
// tests) fall back to manual path entry instead of failing at
// module load. Only path strings travel — secret contents never
// cross IPC or argv, and the picker itself never reads files.

/** Pick one existing file, or `null` where the user cancels or the native picker is unavailable. */
export async function pickExistingFile(title: string): Promise<string | null> {
  try {
    const dialog = await import("@tauri-apps/plugin-dialog");
    const picked: unknown = await dialog.open({
      title,
      multiple: false,
      directory: false,
    });
    if (picked === null || picked === undefined) return null;
    // Single-file pick resolves a path string; anything else
    // (multi-pick arrays, file-info objects on some targets) is
    // not a path the adapter can use — `null` keeps manual entry
    // as the fallback instead of guessing.
    if (typeof picked === "string") return picked;
    if (typeof picked === "object" && "path" in picked && typeof picked.path === "string") {
      return picked.path;
    }
    return null;
  } catch {
    return null;
  }
}
