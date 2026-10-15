import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { vi, test, expect, beforeEach } from "vitest";
import { FileRecovery } from "../src/ui/FileRecovery";
const mock = vi.hoisted(() => ({
  request: vi.fn(),
  on: vi.fn().mockResolvedValue(() => {}),
}));
vi.mock("../src/api", () => mock);
const row = {
  id: "journal",
  kind: "tree_copy",
  path: "C:/fixture/source",
  state: "undone",
  checkpoint: "C:/fixture/checkpoint",
  staging: null,
  parked_target: null,
};
const plan = {
  plan_id: "review",
  artifacts: [
    {
      journal_id: "journal",
      path: "C:/fixture/checkpoint",
      role: "checkpoint",
      bytes: 12,
      entries: 3,
      forgets_recovery: false,
      manual_review: "recorded checkpoint",
    },
  ],
  skipped: [],
};
beforeEach(() => {
  mock.request.mockReset();
  mock.request.mockImplementation(
    async (
      op: string,
      input: { tool?: string; input?: { body?: { action: string } } },
    ) => {
      if (op === "settings.get") return [];
      if (input?.tool === "files.undo_history") return [row];
      if (input?.tool === "files.cleanup_plans") return [];
      if (
        input?.tool === "files.cleanup" &&
        input.input?.body?.action === "preview"
      )
        return plan;
      return {};
    },
  );
});
test("periodic cleanup is opt-in, validates age and saves without submitting an erase", async () => {
  render(<FileRecovery />);
  await screen.findByText(row.path);
  expect(
    screen.getByRole("checkbox", { name: "Preparar lotes automáticamente" }),
  ).not.toBeChecked();
  fireEvent.click(
    screen.getByRole("checkbox", { name: "Preparar lotes automáticamente" }),
  );
  fireEvent.change(
    screen.getByLabelText("Días para revisar copias incompletas"),
    { target: { value: "0" } },
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Guardar mantenimiento" }),
  );
  expect(await screen.findByRole("alert")).toHaveTextContent("1 y 3650");
  expect(mock.request).not.toHaveBeenCalledWith(
    "settings.set",
    expect.anything(),
  );
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Guardar mantenimiento" }),
    ).toBeEnabled(),
  );
  fireEvent.click(
    screen.getByRole("checkbox", { name: "Preparar lotes automáticamente" }),
  );
  fireEvent.change(
    screen.getByLabelText("Días para revisar copias incompletas"),
    { target: { value: "7" } },
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Guardar mantenimiento" }),
  );
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("settings.set", {
      key: "file_maintenance",
      value: { enabled: true, staging_days: 7, include_recovery: false },
    }),
  );
  expect(mock.request).not.toHaveBeenCalledWith(
    "tools.execute",
    expect.objectContaining({ tool: "files.cleanup" }),
  );
});
test("recovery prepares an explicit review and never deletes on selection or option change", async () => {
  render(<FileRecovery />);
  await screen.findByText(row.path);
  expect(
    screen.getByRole("button", { name: "Preparar revisión por antigüedad" }),
  ).toBeDisabled();
  fireEvent.click(
    screen.getByRole("checkbox", { name: `Seleccionar ${row.path}` }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Preparar revisión de limpieza" }),
  );
  await screen.findByRole("button", { name: "Solicitar borrado definitivo" });
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "files.cleanup",
    input: {
      body: {
        action: "preview",
        ids: ["journal"],
        forget_undo: false,
        include_changed: false,
        adopt_legacy: false,
      },
    },
  });
  fireEvent.click(screen.getByRole("checkbox", { name: /Permitir descartar/ }));
  expect(
    screen.queryByRole("button", { name: "Solicitar borrado definitivo" }),
  ).not.toBeInTheDocument();
  expect(mock.request).not.toHaveBeenCalledWith(
    "tools.execute",
    expect.objectContaining({
      input: { body: { action: "apply", plan_id: "review" } },
    }),
  );
});
test("a rejected deletion remains visible and interrupted cleanup uses its persisted plan", async () => {
  const base = mock.request.getMockImplementation()!;
  mock.request.mockImplementation(async (op, input) => {
    if (input?.tool === "files.cleanup_plans")
      return [
        { id: "partial", state: "interrupted", next_index: 2, error: "locked" },
      ];
    if (
      input?.tool === "files.cleanup" &&
      input?.input?.body?.action === "apply"
    )
      throw new Error("PERMISSION_DENIED");
    return base(op, input);
  });
  render(<FileRecovery />);
  await screen.findByRole("button", { name: "Revisar y reanudar limpieza" });
  expect(screen.getByText(/2 artefactos completados/)).toBeInTheDocument();
  fireEvent.click(
    screen.getByRole("button", { name: "Revisar y reanudar limpieza" }),
  );
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "PERMISSION_DENIED",
  );
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "files.cleanup",
    input: { body: { action: "apply", plan_id: "partial" } },
  });
  expect(
    screen.queryByText("Limpieza reanudada y completada."),
  ).not.toBeInTheDocument();
});
test("a pending periodic plan requests a new explicit review under the current configuration", async () => {
  const base = mock.request.getMockImplementation()!;
  mock.request.mockImplementation(async (op, input) => {
    if (input?.tool === "files.cleanup_plans")
      return [
        {
          id: "periodic",
          maintenance: 1,
          state: "prepared",
          next_index: 0,
          error: null,
        },
      ];
    return base(op, input);
  });
  render(<FileRecovery />);
  const button = await screen.findByRole("button", {
    name: "Revisar limpieza pendiente",
  });
  expect(mock.request).not.toHaveBeenCalledWith(
    "tools.execute",
    expect.objectContaining({
      input: {
        body: { action: "apply", plan_id: "periodic", revalidate_policy: true },
      },
    }),
  );
  fireEvent.click(button);
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("tools.execute", {
      tool: "files.cleanup",
      input: {
        body: { action: "apply", plan_id: "periodic", revalidate_policy: true },
      },
    }),
  );
});
test("retention is saved explicitly and clearing it preserves manual selection", async () => {
  render(<FileRecovery />);
  await screen.findByText(row.path);
  fireEvent.change(screen.getByLabelText("Días de retención"), {
    target: { value: "0" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar retención" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("1 y 3650");
  expect(mock.request).not.toHaveBeenCalledWith(
    "settings.set",
    expect.anything(),
  );
  fireEvent.change(screen.getByLabelText("Días de retención"), {
    target: { value: "" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar retención" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("settings.set", {
      key: "file_checkpoint_days",
      value: null,
    }),
  );
  expect(mock.request).not.toHaveBeenCalledWith("tools.execute", {
    tool: "files.retention_preview",
    input: {},
  });
});

test("blocked native recovery requests explicit revalidation and keeps a rejected conflict visible", async () => {
  const base = mock.request.getMockImplementation()!;
  mock.request.mockImplementation(async (op, input) => {
    if (input?.tool === "files.undo_history")
      return [
        { ...row, state: "blocked", revalidation_available: true },
        {
          ...row,
          id: "legacy",
          path: "C:/fixture/legacy",
          state: "blocked",
          revalidation_available: false,
        },
      ];
    if (input?.tool === "files.undo")
      throw new Error("UNDO_DESTINATION_EXISTS");
    return base(op, input);
  });
  render(<FileRecovery />);
  const buttons = await screen.findAllByRole("button", {
    name: "Revalidar y solicitar deshacer",
  });
  expect(buttons).toHaveLength(1);
  expect(mock.request).not.toHaveBeenCalledWith(
    "tools.execute",
    expect.objectContaining({ tool: "files.undo" }),
  );
  fireEvent.click(buttons[0]);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "UNDO_DESTINATION_EXISTS",
  );
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "files.undo",
    input: { id: row.id, revalidate: true },
  });
  expect(screen.queryByText("Operación deshecha.")).not.toBeInTheDocument();
});
test("a recorded write failure stays visible and reconciliation reports a verified recovery state", async () => {
  const base = mock.request.getMockImplementation()!;
  mock.request.mockImplementation(async (op, input) => {
    if (input?.tool === "files.undo_history")
      return [
        {
          ...row,
          kind: "write",
          state: "blocked",
          revalidation_available: true,
          failure_reason: "FILE_WRITE: os=112",
        },
      ];
    if (input?.tool === "files.undo") return { reconciled: true };
    return base(op, input);
  });
  render(<FileRecovery />);
  expect(
    await screen.findByText(/Último fallo: FILE_WRITE: os=112/),
  ).toBeInTheDocument();
  expect(mock.request).not.toHaveBeenCalledWith(
    "tools.execute",
    expect.objectContaining({ tool: "files.undo" }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Revalidar y solicitar deshacer" }),
  );
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Estado de recuperación verificado.",
  );
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "files.undo",
    input: { id: row.id, revalidate: true },
  });
});
