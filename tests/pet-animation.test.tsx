import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { usePetAnimation } from "../src/ui/usePetAnimation";
import { AssetRegistry } from "../src/assets";
import type { PetState } from "../src/types";
const mock = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("../src/api", () => ({ desktop: true, request: mock.request }));
let hidden = false;
let reduced = false;
function Sprite({ state = "idle" }: { state?: PetState }) {
  const style = usePetAnimation(state, () => {});
  return <div data-testid="sprite" style={style} />;
}
beforeEach(() => {
  vi.useFakeTimers();
  hidden = false;
  reduced = false;
  vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
  vi.stubGlobal("matchMedia", () => ({
    get matches() {
      return reduced;
    },
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  }));
  mock.request.mockReset().mockResolvedValue({});
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
test("a state change waits for the previous native mask and never paints a stale frame", async () => {
  let release!: () => void;
  mock.request.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        release = resolve;
      }),
  );
  const view = render(<Sprite />);
  await act(async () => {});
  view.rerender(<Sprite state="hover" />);
  await act(async () => {
    release();
  });
  expect(mock.request.mock.calls.map((c) => c[1])).toEqual([
    { state: "idle", frame: 0 },
    { state: "hover", frame: 0 },
  ]);
  expect(view.getByTestId("sprite").style.backgroundImage).toContain(
    AssetRegistry.pet.hover.source,
  );
  view.unmount();
  const calls = mock.request.mock.calls.length;
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10000);
  });
  expect(mock.request).toHaveBeenCalledTimes(calls);
});
test("hidden windows pause native updates and resume from a known first frame", async () => {
  const view = render(<Sprite />);
  await act(async () => {});
  hidden = true;
  await act(async () => {
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(10000);
  });
  expect(mock.request).toHaveBeenCalledTimes(1);
  hidden = false;
  await act(async () => {
    document.dispatchEvent(new Event("visibilitychange"));
  });
  expect(mock.request).toHaveBeenLastCalledWith("pet.frame", {
    state: "idle",
    frame: 0,
  });
  expect(mock.request).toHaveBeenCalledTimes(2);
  view.unmount();
});
test("reduced motion displays a static first frame without a repeating timer", async () => {
  reduced = true;
  const view = render(<Sprite />);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10000);
  });
  expect(mock.request).toHaveBeenCalledTimes(1);
  view.unmount();
});
