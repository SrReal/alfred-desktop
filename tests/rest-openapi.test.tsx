import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { RestOpenApi } from "../src/ui/RestOpenApi";
const mock = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("../src/api", () => mock);
const expected = { fingerprint: "a".repeat(64) };
const proposal = {
  expected,
  preview: {
    version: "3.2.1",
    profile: {
      profile_version: 1,
      read_only: true,
      auth: { type: "none" },
      operations: {
        health: { method: "GET", path: "/health", risk: "free" },
        write: { method: "POST", path: "/write", risk: "mandatory" },
      },
    },
    diagnostics: [
      {
        method: "POST",
        path: "/unsupported",
        code: "OPENAPI_UNSUPPORTED_CONTENT",
      },
    ],
    operations_seen: 3,
    operations_proposed: 2,
  },
};
beforeEach(() => {
  mock.request.mockReset();
  mock.request.mockResolvedValue(proposal);
});
test("OpenAPI selection is explicit and apply uses the exact preview fingerprint", async () => {
  const applied = vi.fn();
  render(<RestOpenApi connection="owned-id" onApplied={applied} />);
  fireEvent.click(screen.getByRole("button", { name: "Revisar OpenAPI" }));
  await screen.findByLabelText("Seleccionar health");
  expect(
    screen.getByRole("button", { name: "Guardar operaciones seleccionadas" }),
  ).toBeDisabled();
  expect(screen.getByText(/OPENAPI_UNSUPPORTED_CONTENT/)).toBeInTheDocument();
  fireEvent.click(screen.getByLabelText("Seleccionar health"));
  fireEvent.click(
    screen.getByRole("button", { name: "Guardar operaciones seleccionadas" }),
  );
  await waitFor(() =>
    expect(applied).toHaveBeenCalledWith(
      expect.objectContaining({
        read_only: true,
        operations: { health: proposal.preview.profile.operations.health },
      }),
    ),
  );
  expect(mock.request).toHaveBeenLastCalledWith("tools.execute", {
    tool: "rest.import_openapi",
    input: {
      connection: "owned-id",
      action: "apply",
      expected,
      operations: ["health"],
    },
  });
});
test("a changed inline document invalidates selection and an old response cannot apply", async () => {
  const applied = vi.fn();
  let resolve: ((v: typeof proposal) => void) | undefined;
  mock.request.mockImplementation(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const view = render(
    <RestOpenApi connection="owned-id" onApplied={applied} />,
  );
  fireEvent.click(
    screen.getByRole("checkbox", {
      name: "Consultar openapi_path de esta conexión",
    }),
  );
  fireEvent.change(screen.getByLabelText("Documento OpenAPI JSON"), {
    target: { value: '{"openapi":"3.2.1"}' },
  });
  fireEvent.click(screen.getByRole("button", { name: "Revisar OpenAPI" }));
  expect(mock.request).toHaveBeenLastCalledWith("tools.execute", {
    tool: "rest.import_openapi",
    input: {
      connection: "owned-id",
      action: "preview",
      document: { openapi: "3.2.1" },
    },
  });
  view.unmount();
  resolve?.(proposal);
  await waitFor(() => expect(applied).not.toHaveBeenCalled());
});
test("an apply error requires a new preview and retains the error", async () => {
  render(<RestOpenApi connection="owned-id" onApplied={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Revisar OpenAPI" }));
  await screen.findByLabelText("Seleccionar health");
  fireEvent.click(screen.getByLabelText("Seleccionar health"));
  mock.request.mockRejectedValueOnce(new Error("REST_PROFILE_CHANGED"));
  fireEvent.click(
    screen.getByRole("button", { name: "Guardar operaciones seleccionadas" }),
  );
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "REST_PROFILE_CHANGED",
  );
  expect(screen.queryByLabelText("Seleccionar health")).not.toBeInTheDocument();
});
