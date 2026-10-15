import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { SystemAudit } from "../src/ui/SystemAudit";
import { RememberedPermissions } from "../src/ui/RememberedPermissions";
const api = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => api);
beforeEach(() => {
  api.request.mockReset();
  api.on.mockResolvedValue(() => {});
});
test("audit is explicit, displays unavailable checks and does not start a scan automatically", async () => {
  const report = {
    checkedAt: "2026-10-13T10:00:00Z",
    scope: "Sólo observaciones",
    sections: [
      {
        name: "Microsoft Defender",
        status: "unavailable",
        error: "AccessDenied",
      },
    ],
  };
  api.request.mockImplementation(async (operation) =>
    operation === "system_audit.list" ? [] : report,
  );
  render(<SystemAudit />);
  expect(api.request).not.toHaveBeenCalledWith(
    "tools.execute",
    expect.anything(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Auditar equipo" }));
  await screen.findByText("No disponible: AccessDenied");
  expect(
    api.request.mock.calls.filter(([op]) => op === "tools.execute"),
  ).toHaveLength(1);
  expect(api.request).toHaveBeenCalledWith("tools.execute", {
    tool: "windows.audit",
    input: { elevated: false },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Auditar con autorización UAC" }),
  );
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("tools.execute", {
      tool: "windows.audit",
      input: { elevated: true },
    }),
  );
});
test("admin repair is a selected request, UAC failure remains visible with no retry", async () => {
  api.request.mockImplementation(async (op) => {
    if (op === "system_audit.list") return [];
    throw new Error("ADMIN_UAC_CANCELLED_OR_FAILED");
  });
  render(<SystemAudit />);
  fireEvent.change(
    screen.getByRole("combobox", { name: "Operación administrativa" }),
    {
      target: { value: "sfc_repair" },
    },
  );
  fireEvent.click(screen.getByRole("button", { name: "Revisar y ejecutar" }));
  await screen.findByRole("alert");
  expect(
    api.request.mock.calls.filter(([op]) => op === "tools.execute"),
  ).toHaveLength(1);
  expect(api.request).toHaveBeenCalledWith("tools.execute", {
    tool: "windows.admin_action",
    input: { action: "sfc_repair" },
  });
  expect(
    screen.getByRole("button", { name: "Revisar y ejecutar" }),
  ).toBeEnabled();
});
test("remembered grants show the exact scope and revocation sends only its id", async () => {
  api.request
    .mockResolvedValueOnce([
      {
        id: "rule-1",
        tool_id: "terminal.command",
        scope_json: JSON.stringify({
          preview: { command: "Get-Service", elevated: false },
        }),
      },
    ])
    .mockResolvedValueOnce({ revoked: true })
    .mockResolvedValue([]);
  render(<RememberedPermissions />);
  await screen.findByText("terminal.command");
  expect(screen.getByText(/Get-Service/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Revocar permiso" }));
  await screen.findByText("No hay permisos recordados.");
  expect(api.request).toHaveBeenCalledWith("permissions.revoke", {
    id: "rule-1",
  });
});
