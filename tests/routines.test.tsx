import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { request } from "../src/api";
import { Routines } from "../src/ui/Routines";
vi.mock("../src/api", () => ({
  request: vi.fn(),
  on: vi.fn(async () => () => {}),
}));
const call = vi.mocked(request);
const routine = {
  id: "routine",
  revision: "review-1",
  name: "Preparar informe",
  macroId: "macro",
  available: true,
  body: { read: false },
};
beforeEach(() => {
  call.mockReset();
});
function baseline() {
  call.mockImplementation(async (operation) =>
    operation === "routines.list" ? [routine] : null,
  );
}
describe("routine shortcuts", () => {
  it("does not execute when loading or refreshing and requires confirmation to remove only the shortcut", async () => {
    baseline();
    render(<Routines query="" />);
    await screen.findByRole("button", {
      name: "Ejecutar rutina Preparar informe",
    });
    fireEvent.click(screen.getByText("Actualizar rutinas"));
    await waitFor(() => expect(call).toHaveBeenCalledTimes(4));
    expect(
      call.mock.calls.every(([op]) =>
        ["routines.list", "routines.status"].includes(op),
      ),
    ).toBe(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Quitar rutina Preparar informe" }),
    );
    expect(call).toHaveBeenCalledTimes(4);
    fireEvent.click(screen.getByText("Confirmar quitar rutina"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("routines.delete", {
        id: routine.id,
        revision: routine.revision,
        confirmed: true,
      }),
    );
    expect(call.mock.calls.some(([op]) => op === "tools.execute")).toBe(false);
  });
  it("prevents duplicate starts and recovers running status after remount", async () => {
    let state: unknown = null;
    let finish!: () => void;
    call.mockImplementation(async (operation) => {
      if (operation === "routines.list") return [routine];
      if (operation === "routines.status") return state;
      if (operation === "routines.run") {
        state = {
          id: routine.id,
          name: routine.name,
          status: "running",
          startedAt: "2026-10-14T07:00:00Z",
        };
        return new Promise<void>((resolve) => {
          finish = resolve;
        });
      }
    });
    const view = render(<Routines query="" />);
    const button = await screen.findByRole("button", {
      name: "Ejecutar rutina Preparar informe",
    });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(
      call.mock.calls.filter(([op]) => op === "routines.run"),
    ).toHaveLength(1);
    expect(call).toHaveBeenCalledWith("routines.run", {
      id: routine.id,
      revision: routine.revision,
    });
    view.unmount();
    render(<Routines query="" />);
    await screen.findByText(/En curso o esperando autorización/);
    expect(
      screen.getByRole("button", { name: "Ejecutar rutina Preparar informe" }),
    ).toBeDisabled();
    finish();
  });
  it("disables changed routines and leaves failures visible without retrying", async () => {
    call.mockImplementation(async (operation) => {
      if (operation === "routines.list")
        return [
          {
            ...routine,
            available: false,
            issue: "MACRO_CHANGED_REVIEW_REQUIRED",
          },
        ];
      return null;
    });
    const view = render(<Routines query="" />);
    expect(
      await screen.findByRole("button", {
        name: "Ejecutar rutina Preparar informe",
      }),
    ).toBeDisabled();
    await screen.findByText(/MACRO_CHANGED_REVIEW_REQUIRED/);
    view.unmount();
    baseline();
    call.mockImplementation(async (operation) => {
      if (operation === "routines.list") return [routine];
      if (operation === "routines.run")
        throw new Error("La rutina ha cambiado");
      return null;
    });
    render(<Routines query="" />);
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Ejecutar rutina Preparar informe",
      }),
    );
    await screen.findByRole("alert");
    expect(
      call.mock.calls.filter(([op]) => op === "routines.run"),
    ).toHaveLength(1);
  });
});
