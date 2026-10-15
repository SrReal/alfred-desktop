import type { PetState } from "./types";
import production from "../assets/pixellab/production/runtime.json";
const images = import.meta.glob(
  "../assets/pixellab/production/{pet,ui,icons}/*.png",
  {
    eager: true,
    query: "?url",
    import: "default",
  },
) as Record<string, string>;
function source(path: string): string {
  const url = images[`../assets/pixellab/production/${path}`];
  if (!url) throw new Error(`Missing visual asset: ${path}`);
  return url;
}
export const petStates: PetState[] = [
  "idle",
  "hover",
  "dragging",
  "falling",
  "thinking",
  "working",
  "searching_web",
  "looking_at_screen",
  "using_pc",
  "waiting_permission",
  "waiting_user",
  "success",
  "error",
  "privacy",
  "dnd",
  "sleeping",
];
export interface SpriteAsset {
  source: string;
  state: PetState;
  width: number;
  height: number;
  baseline: number;
  frames: number;
  durations: number[];
  loop: boolean;
  hitRegion: { x: number; y: number; width: number; height: number };
}
export interface UiAsset {
  source: string;
  width: number;
  height: number;
  nineSlice: number[] | null;
}
const petSources = production.pet as Record<string, Omit<SpriteAsset, "state">>;
const ui = Object.fromEntries(
  Object.entries(production.ui).map(([name, asset]) => [
    name,
    { ...asset, source: source(asset.source) },
  ]),
) as Record<string, UiAsset>;
export const AssetRegistry = {
  pixellabEnabled: true,
  animatedStates: Object.keys(petSources) as PetState[],
  pet: Object.fromEntries(
    petStates.map((state) => {
      const key = petSources[state] ? state : "idle";
      const asset = petSources[key];
      return [state, { ...asset, state: key, source: source(asset.source) }];
    }),
  ) as Record<PetState, SpriteAsset>,
  icons: Object.fromEntries(
    Object.entries(production.icons).map(([name, path]) => [
      name,
      source(path),
    ]),
  ) as Record<string, string>,
  ui,
  panel: ui.panel,
  button: ui.button,
};
export const visualStyle = Object.fromEntries(
  Object.entries(ui).map(([name, asset]) => [
    `--${name}-image`,
    `url(${asset.source})`,
  ]),
);
export const tokens = {
  petWidth: 144,
  petHeight: 192,
  compactWidth: 144,
  compactHeight: 192,
  gravity: 2000,
  maxFallSpeed: 1800,
  maxFallMs: 1500,
  anchorGap: 12,
};
export function gravityFrame(
  from: number,
  to: number,
  elapsedMs: number,
): number {
  if (from >= to) return to;
  const t = Math.max(0, elapsedMs) / 1000;
  return Math.min(
    to,
    from + Math.min((tokens.gravity * t * t) / 2, tokens.maxFallSpeed * t),
  );
}
export function audioAllowed(event: string, dnd: boolean): boolean {
  return (
    [
      "waiting_permission",
      "waiting_user",
      "completed_requires_attention",
      "background_task_failed",
    ].includes(event) &&
    (!dnd ||
      event === "waiting_permission" ||
      event === "background_task_failed")
  );
}
