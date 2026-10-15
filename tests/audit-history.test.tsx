import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { request } from "../src/api";
import { AuditHistory } from "../src/ui/AuditHistory";
vi.mock("../src/api", () => ({ request: vi.fn() }));
const call = vi.mocked(request);
const origin = { id: "fixture-origin", computer: "TEST-PC" };
const old = {
  id: "old",
  checkedAt: "2026-10-12T10:00:00Z",
  origin,
  elevated: false,
  unavailable: 0,
};
const recent = {
  ...old,
  id: "new",
  checkedAt: "2026-10-13T10:00:00Z",
  elevated: true,
};
const report = { ...recent, scope: "Observaciones", sections: [] };
const diff = {
  before: old,
  after: recent,
  changedSections: 1,
  incompleteSections: 1,
  differentElevation: true,
  sections: [
    {
      name: "Microsoft Defender",
      source: "Get-MpComputerStatus",
      status: "changed",
      note: "Estado observado",
      changes: [
        {
          label: "Protección en tiempo real",
          explanation: "Sí → No",
          kind: "changed",
          before: true,
          after: false,
        },
      ],
    },
    {
      name: "Servicios",
      source: "Win32_Service",
      status: "unavailable",
      beforeStatus: "observed",
      afterStatus: "unavailable",
      note: "Muestra acotada",
      changes: [],
    },
  ],
};
beforeEach(() => {
  call.mockReset();
  call.mockImplementation(async (op) =>
    op === "system_audit.list"
      ? [recent, old]
      : op === "system_audit.compare"
        ? diff
        : report,
  );
});
test("loads only saved summaries and compares explicit IDs with dated sources and incomplete checks", async () => {
  render(<AuditHistory revision={0} disabled={false} onReport={vi.fn()} />);
  const compare = await screen.findByRole("button", {
    name: "Comparar auditorías",
  });
  await waitFor(() => expect(compare).toBeEnabled());
  expect(call).toHaveBeenCalledTimes(1);
  fireEvent.click(compare);
  await screen.findByRole("region", { name: "Resultado de comparación" });
  expect(call).toHaveBeenLastCalledWith("system_audit.compare", {
    beforeId: "old",
    afterId: "new",
  });
  expect(screen.getByText(/permisos distintos/)).toBeInTheDocument();
  expect(screen.getByText(/Fuente: Get-MpComputerStatus/)).toBeInTheDocument();
  expect(
    screen.getByText(/No se deducen altas, bajas ni mejoras/),
  ).toBeInTheDocument();
  expect(
    screen.getByText(/Protección en tiempo real · Sí → No/),
  ).toBeInTheDocument();
  expect(call.mock.calls.some(([op]) => op === "tools.execute")).toBe(false);
  fireEvent.change(screen.getByLabelText("Auditoría anterior"), {
    target: { value: "new" },
  });
  expect(compare).toBeDisabled();
  expect(
    screen.queryByRole("region", { name: "Resultado de comparación" }),
  ).not.toBeInTheDocument();
});
test("view restores report and delete requires confirmation scoped to the chosen snapshot", async () => {
  const onReport = vi.fn();
  render(<AuditHistory revision={0} disabled={false} onReport={onReport} />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Ver auditoría reciente" }),
  );
  await waitFor(() => expect(onReport).toHaveBeenCalledWith(report));
  fireEvent.click(
    screen.getByRole("button", { name: "Borrar auditoría reciente" }),
  );
  expect(call.mock.calls.some(([op]) => op === "system_audit.delete")).toBe(
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "Cancelar borrado" }));
  expect(
    screen.queryByRole("button", { name: "Confirmar borrado" }),
  ).not.toBeInTheDocument();
  fireEvent.click(
    screen.getByRole("button", { name: "Borrar auditoría reciente" }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Confirmar borrado" }));
  await waitFor(() =>
    expect(call).toHaveBeenCalledWith("system_audit.delete", {
      id: "new",
      confirmed: true,
    }),
  );
  await waitFor(() => expect(onReport).toHaveBeenCalledWith(undefined));
});
test("failed comparisons remove the previous result and expose the native rejection", async () => {
  render(<AuditHistory revision={0} disabled={false} onReport={vi.fn()} />);
  const button = await screen.findByRole("button", {
    name: "Comparar auditorías",
  });
  fireEvent.click(button);
  await screen.findByRole("region", { name: "Resultado de comparación" });
  call.mockRejectedValueOnce(
    new Error("Selecciona auditorías del mismo origen"),
  );
  fireEvent.click(button);
  await screen.findByRole("alert");
  expect(
    screen.queryByRole("region", { name: "Resultado de comparación" }),
  ).not.toBeInTheDocument();
});
test("a pending old list cannot overwrite a refreshed history", async () => {
  let settle: (v: unknown) => void = () => {};
  call.mockImplementationOnce(
    () =>
      new Promise((r) => {
        settle = r;
      }),
  );
  const props = { disabled: false, onReport: vi.fn() };
  const view = render(<AuditHistory {...props} revision={0} />);
  view.rerender(<AuditHistory {...props} revision={1} />);
  await screen.findByRole("button", { name: "Comparar auditorías" });
  await act(async () => settle([]));
  expect(screen.getByLabelText("Auditoría reciente")).toHaveValue("new");
});
test("empty history and errors keep refresh available without running audits", async () => {
  call.mockResolvedValueOnce([]);
  render(<AuditHistory revision={0} disabled={false} onReport={vi.fn()} />);
  await screen.findByText(/No hay auditorías guardadas/);
  call.mockRejectedValueOnce(new Error("AUDIT_HISTORY_INVALID"));
  fireEvent.click(screen.getByRole("button", { name: "Actualizar historial" }));
  await screen.findByRole("alert");
  expect(
    screen.getByRole("button", { name: "Actualizar historial" }),
  ).toBeEnabled();
});
