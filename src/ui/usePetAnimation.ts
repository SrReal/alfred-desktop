import { useEffect, useRef, useState, type CSSProperties } from "react";
import { AssetRegistry } from "../assets";
import { desktop, request } from "../api";
import type { PetState } from "../types";

// Serializes native mask updates across React effect cleanup and state changes.
let paintQueue: Promise<void> = Promise.resolve();
export function usePetAnimation(
  state: PetState,
  onError: (message: string) => void,
): CSSProperties {
  const [paint, setPaint] = useState({ state: "idle" as PetState, frame: 0 });
  const errorHandler = useRef(onError);
  errorHandler.current = onError;
  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const motion =
      typeof matchMedia === "function"
        ? matchMedia("(prefers-reduced-motion: reduce)")
        : null;
    const asset = AssetRegistry.pet[state];
    let frame = 0;
    let cycle = 0;
    function tick() {
      if (!live || document.hidden) return;
      const generation = cycle;
      const current = motion?.matches ? 0 : frame;
      paintQueue = paintQueue
        .catch(() => {})
        .then(async () => {
          if (!live || document.hidden || generation !== cycle) return;
          if (desktop)
            await request("pet.frame", { state: asset.state, frame: current });
          if (!live || generation !== cycle) return;
          setPaint({ state, frame: current });
          if (motion?.matches || (!asset.loop && current === asset.frames - 1))
            return;
          frame = (current + 1) % asset.frames;
          timer = setTimeout(tick, asset.durations[current]);
        })
        .catch((error: unknown) => {
          if (live)
            errorHandler.current(
              error instanceof Error ? error.message : String(error),
            );
        });
    }
    function restart() {
      clearTimeout(timer);
      cycle++;
      frame = 0;
      tick();
    }
    document.addEventListener("visibilitychange", restart);
    motion?.addEventListener("change", restart);
    tick();
    return () => {
      live = false;
      clearTimeout(timer);
      document.removeEventListener("visibilitychange", restart);
      motion?.removeEventListener("change", restart);
    };
  }, [state]);
  const asset = AssetRegistry.pet[paint.state];
  return {
    backgroundImage: `url(${asset.source})`,
    backgroundSize: `${asset.frames * 100}% 100%`,
    backgroundPosition: `${asset.frames > 1 ? (paint.frame * 100) / (asset.frames - 1) : 0}% 0`,
  };
}
