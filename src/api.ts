import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Json } from "./types";
export const desktop = isTauri();
export async function request<T>(
  operation: string,
  input: Json = {},
): Promise<T> {
  if (!desktop) {
    if (operation === "dictation.state") return null as T;
    if (operation === "explain.pending") return null as T;
    if (operation === "clipboard.status")
      return {
        enabled: false,
        module_enabled: false,
        privacy: false,
        scope_generation: 0,
        entries: 0,
        maximum_entries: 100,
        retention_days: 7,
        last_error: null,
      } as T;
    if (
      operation === "tools.execute" &&
      typeof input === "object" &&
      input !== null &&
      !Array.isArray(input) &&
      ["tasks.list", "tasks.history"].includes(String(input.tool))
    )
      return [] as T;
    if (operation === "pet_drop.list")
      return { items: [], last_error: null } as T;
    if (operation === "semantic.status")
      return {
        available: false,
        enabled: false,
        installed: false,
        indexed: 0,
        bytes: 127540686,
        job: null,
        last_error: null,
      } as T;
    if (
      operation.endsWith(".list") ||
      operation === "settings.get" ||
      operation === "memory.candidates" ||
      operation === "memory.analyses" ||
      operation === "search"
    )
      return [] as T;
    if (operation === "claude.health")
      return {
        approved: false,
        diagnostic:
          "Vista previa. Inicia AlfredPet para usar Claude y herramientas.",
        version: "LIVE_NOT_RUN",
      } as T;
    throw new Error(
      "Esta acción necesita AlfredPet de escritorio. La vista web sólo muestra la interfaz.",
    );
  }
  return invoke<T>("request", { operation, input });
}
export async function on<T>(
  event: string,
  callback: (payload: T) => void,
): Promise<() => void> {
  if (!desktop) return () => {};
  return listen<T>(event, (e) => callback(e.payload));
}
export async function show(surface: string): Promise<void> {
  await request("window.show", { surface });
}
