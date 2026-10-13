import { formatError } from "../ui/formatError";

/**
 * Whether GitPulse starts at login. `unknown` is a state of its own: a plugin
 * that throws (unsupported platform, permission, a build without it) used to
 * render as "Off", which tells the reader the setting is safe to rely on when
 * nobody could read it.
 */
export type LaunchAtLogin =
  | { state: "on" | "off" }
  | { state: "unknown"; error: string };

export type Autostart = {
  isEnabled: () => Promise<boolean>;
  enable: () => Promise<void>;
  disable: () => Promise<void>;
};

const loadPlugin = (): Promise<Autostart> => import("@tauri-apps/plugin-autostart");

const fromEnabled = (enabled: boolean): LaunchAtLogin => ({ state: enabled ? "on" : "off" });

export async function readLaunchAtLogin(load: () => Promise<Autostart> = loadPlugin): Promise<LaunchAtLogin> {
  try {
    return fromEnabled(await (await load()).isEnabled());
  } catch (error) {
    return { state: "unknown", error: formatError(error) };
  }
}

/**
 * Applies the change, then reports what the operating system says. When the
 * change fails the answer is the state actually read back (or `unknown` when
 * even that cannot be read) with the failure attached, never a guess.
 */
export async function writeLaunchAtLogin(next: boolean, load: () => Promise<Autostart> = loadPlugin): Promise<LaunchAtLogin> {
  try {
    const plugin = await load();
    if (next) await plugin.enable();
    else await plugin.disable();
    return fromEnabled(await plugin.isEnabled());
  } catch (error) {
    const actual = await readLaunchAtLogin(load);
    return actual.state === "unknown" ? { state: "unknown", error: formatError(error) } : actual;
  }
}
