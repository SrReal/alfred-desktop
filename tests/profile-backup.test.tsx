import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { request } from "../src/api";
import { ProfileBackup } from "../src/ui/ProfileBackup";
vi.mock("../src/api", () => ({ request: vi.fn() }));
const call = vi.mocked(request);
beforeEach(() => {
  call.mockReset();
  call.mockImplementation(async (op) => {
    if (op === "profile.status") return null;
    if (op === "paths.pick") return ["D:\\Copias\\Alfred.alfredbackup"];
    if (op.startsWith("profile.preview_"))
      return {
        counts: { conversations: 3, messages: 12 },
        sha256: "abc",
        created_at: "2026-10-13T12:00:00Z",
        files: 10,
        bytes: 1024,
      };
    return { queued: true };
  });
});
test("requires file choice, preview and explicit confirmation before scheduling a restart", async () => {
  render(<ProfileBackup />);
  expect(
    screen.getByRole("button", { name: "Revisar contenido" }),
  ).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Guardar copia como…" }));
  await screen.findByText("Alfred.alfredbackup");
  fireEvent.click(screen.getByRole("button", { name: "Revisar contenido" }));
  await screen.findByText("Contenido del perfil actual");
  const submit = screen.getByRole("button", {
    name: "Reiniciar y crear copia",
  });
  expect(submit).toBeDisabled();
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(submit);
  await waitFor(() => expect(call).toHaveBeenCalledWith("profile.restart"));
  expect(call).toHaveBeenCalledWith(
    "profile.schedule",
    expect.objectContaining({
      mode: "backup",
      confirmed: true,
      path: "D:\\Copias\\Alfred.alfredbackup",
    }),
  );
  expect(screen.getByRole("status")).toHaveTextContent("Operación preparada");
});
test("changing selection invalidates restore proof; cancelling never schedules", async () => {
  render(<ProfileBackup />);
  fireEvent.click(screen.getByRole("button", { name: "Restaurar copia" }));
  fireEvent.click(screen.getByRole("button", { name: "Seleccionar copia…" }));
  await screen.findByText("Alfred.alfredbackup");
  fireEvent.click(screen.getByRole("button", { name: "Revisar contenido" }));
  await screen.findByText("Copia comprobada");
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(screen.getByRole("button", { name: "Seleccionar copia…" }));
  await waitFor(() =>
    expect(screen.queryByText("Copia comprobada")).not.toBeInTheDocument(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Revisar contenido" }));
  await screen.findByText("Copia comprobada");
  fireEvent.click(screen.getByRole("button", { name: "Cancelar" }));
  expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
  expect(call.mock.calls.some(([op]) => op === "profile.schedule")).toBe(false);
});
test("a failed integrity check never exposes confirmation and remains retryable", async () => {
  call.mockImplementation(async (op) => {
    if (op === "profile.status") return null;
    if (op === "paths.pick") return ["D:\\bad.alfredbackup"];
    throw new Error("PROFILE_HASH_MISMATCH");
  });
  render(<ProfileBackup />);
  fireEvent.click(screen.getByRole("button", { name: "Restaurar copia" }));
  fireEvent.click(screen.getByRole("button", { name: "Seleccionar copia…" }));
  await screen.findByText("bad.alfredbackup");
  fireEvent.click(screen.getByRole("button", { name: "Revisar contenido" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "PROFILE_HASH_MISMATCH",
  );
  expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Revisar contenido" }),
  ).toBeEnabled();
});
test("recovery archive is offered for review without silently starting a restore", async () => {
  call.mockImplementation(async (op) =>
    op === "profile.status"
      ? {
          status: "completed",
          operation: "restore",
          at: "2026-10-13T12:00:00Z",
          result: { recovery_archive: "D:\\anterior.alfredbackup" },
        }
      : { counts: { messages: 5 } },
  );
  render(<ProfileBackup />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Revisar copia anterior" }),
  );
  expect(screen.getByText("anterior.alfredbackup")).toBeInTheDocument();
  expect(call.mock.calls.every(([op]) => op === "profile.status")).toBe(true);
});
