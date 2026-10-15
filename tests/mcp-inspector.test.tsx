import {
  render,
  screen,
  fireEvent,
  waitFor,
  act,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { McpInspector } from "../src/ui/McpInspector";
const request = vi.hoisted(() => vi.fn());
vi.mock("../src/api", () => ({ request }));
const id = "11111111-1111-4111-8111-111111111111";
const expected = { fingerprint: "a".repeat(64) };
beforeEach(() => {
  request.mockReset();
});
test("unknown OAuth outcome requires explicit acknowledgement and never restarts automatically", async () => {
  const issuer = "https://owned-as.invalid";
  request.mockImplementation(
    async (operation: string, value: { tool?: string }) => {
      if (operation === "connections.mcp_oauth_status")
        return {
          connection: id,
          session_id: id,
          phase: "uncertain",
          cleanup_pending: true,
        };
      return {
        connection: id,
        session_id: id,
        phase: value.tool === "mcp.oauth_reset" ? "cancelled" : "uncertain",
        cleanup_pending: false,
      };
    },
  );
  render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{
        transport: "streamable_http",
        auth: { type: "oauth", issuer },
      }}
      onSaved={vi.fn()}
    />,
  );
  const reset = await screen.findByRole("button", {
    name: "Cerrar intento OAuth desconocido MCP",
  });
  expect(reset).toBeDisabled();
  fireEvent.click(
    screen.getByRole("button", { name: "Limpiar recibos antiguos MCP" }),
  );
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith("tools.execute", {
      tool: "mcp.oauth_cleanup",
      input: { connection: id, session_id: id },
    }),
  );
  fireEvent.click(
    screen.getByLabelText("Aceptar desenlace OAuth desconocido MCP"),
  );
  fireEvent.click(
    screen.getByRole("button", {
      name: "Cerrar intento OAuth desconocido MCP",
    }),
  );
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith("tools.execute", {
      tool: "mcp.oauth_reset",
      input: {
        connection: id,
        session_id: id,
        acknowledge_remote_unknown: true,
      },
    }),
  );
  expect(
    request.mock.calls.some(([, v]) => v?.tool === "mcp.oauth_start"),
  ).toBe(false);
  expect(
    request.mock.calls.some(
      ([operation]) => operation === "connections.mcp_oauth",
    ),
  ).toBe(false);
});
test("OAuth login uses private credential input and clears it after submitting", async () => {
  const issuer = "https://owned-as.invalid/tenant";
  request.mockImplementation(
    async (operation: string, value: { tool?: string }) => {
      if (operation === "connections.mcp_oauth_status")
        return { connection: id, phase: "not_started" };
      if (operation === "connections.mcp_oauth")
        return { connection: id, session_id: id, phase: "authorizing" };
      if (value.tool === "mcp.oauth_discover")
        return {
          issuer,
          requested_scopes: ["files:read"],
          alfredpetObservation: expected,
        };
      return { registrationPerformed: false };
    },
  );
  render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{
        transport: "streamable_http",
        auth: { type: "oauth", issuer },
      }}
      onSaved={vi.fn()}
    />,
  );
  await waitFor(() =>
    expect(screen.getByLabelText("Emisor OAuth MCP")).toHaveValue(issuer),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Consultar autorización MCP" }),
  );
  await screen.findByLabelText("Registro del cliente MCP");
  fireEvent.change(screen.getByLabelText("Registro del cliente MCP"), {
    target: { value: "pre_registered" },
  });
  fireEvent.change(screen.getByLabelText("Identificador del cliente MCP"), {
    target: { value: "owned-client" },
  });
  fireEvent.change(screen.getByLabelText("Autenticación del cliente MCP"), {
    target: { value: "client_secret_basic" },
  });
  fireEvent.change(screen.getByLabelText("Secreto del cliente MCP"), {
    target: { value: "owned-secret-input" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Revisar registro MCP" }));
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith(
      "tools.execute",
      expect.objectContaining({ tool: "mcp.oauth_registration" }),
    ),
  );
  const review = request.mock.calls.find(
    ([, value]) => value?.tool === "mcp.oauth_registration",
  );
  expect(JSON.stringify(review)).not.toContain("owned-secret-input");
  fireEvent.click(screen.getByRole("button", { name: "Iniciar sesión MCP" }));
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith("connections.mcp_oauth", {
      input: {
        connection: id,
        issuer,
        expected,
        client_auth: "client_secret_basic",
        requested_scopes: ["files:read"],
        preregistered: {
          client_id: "owned-client",
          redirect_uri: "http://127.0.0.1/oauth/mcp/callback",
        },
        allow_legacy_dynamic: false,
      },
      client_secret: "owned-secret-input",
    }),
  );
  expect(screen.getByLabelText("Secreto del cliente MCP")).toHaveValue("");
  expect(
    await screen.findByText("Esperando autorización en el navegador"),
  ).toBeInTheDocument();
  fireEvent.click(
    screen.getByRole("button", { name: "Cancelar autorización MCP" }),
  );
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith("tools.execute", {
      tool: "mcp.oauth_cancel",
      input: { connection: id, session_id: id },
    }),
  );
});
test("saving an OAuth issuer clears discovery and requires a fresh saved profile", async () => {
  const onSaved = vi.fn();
  const profile = { transport: "streamable_http", auth: { type: "none" } };
  request.mockResolvedValue({ id });
  render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={profile}
      onSaved={onSaved}
    />,
  );
  fireEvent.change(screen.getByLabelText("Emisor OAuth MCP"), {
    target: { value: "https://owned-as.invalid" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar emisor OAuth" }));
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith("connections.save", {
      id,
      module_id: "mcp",
      name: "Owned",
      expected_profile: profile,
      config: {
        transport: "streamable_http",
        auth: { type: "oauth", issuer: "https://owned-as.invalid" },
      },
    }),
  );
  expect(onSaved).toHaveBeenCalled();
  expect(
    screen.queryByRole("button", { name: "Iniciar sesión MCP" }),
  ).not.toBeInTheDocument();
});
test("MCP inspector only executes an explicitly selected owned observation", async () => {
  request.mockImplementation(async (_: string, v: { tool: string }) =>
    v.tool === "mcp.tools"
      ? {
          tools: [
            {
              name: "write",
              alfredpetPermission: "allowed",
              alfredpetObservation: expected,
            },
            { name: "denied", alfredpetPermission: "denied" },
          ],
        }
      : { content: [] },
  );
  render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{}}
      onSaved={vi.fn()}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Consultar lista MCP" }));
  await screen.findByRole("button", { name: "Preparar write" });
  expect(
    screen.queryByRole("button", { name: "Preparar denied" }),
  ).not.toBeInTheDocument();
  expect(request).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Preparar write" }));
  fireEvent.change(screen.getByLabelText("Argumentos MCP"), {
    target: { value: '{"value":"Señor 🐾"}' },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Solicitar ejecución MCP" }),
  );
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith("tools.execute", {
      tool: "mcp.call",
      input: {
        connection: id,
        name: "write",
        body: { value: "Señor 🐾" },
        expected,
      },
    }),
  );
});
test("allowing a subject saves exact local authority and clears stale observations", async () => {
  const onSaved = vi.fn();
  request
    .mockResolvedValueOnce({
      tools: [{ name: "item", alfredpetPermission: "denied" }],
    })
    .mockResolvedValue({ id });
  render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{
        profile_version: 1,
        permissions: { tools: { old: true }, resources: [], prompts: {} },
      }}
      onSaved={onSaved}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Consultar lista MCP" }));
  fireEvent.click(await screen.findByRole("button", { name: "Permitir item" }));
  await waitFor(() =>
    expect(onSaved).toHaveBeenCalledWith(
      expect.objectContaining({
        permissions: {
          tools: { old: true, item: true },
          resources: [],
          prompts: {},
        },
      }),
    ),
  );
  expect(request).toHaveBeenLastCalledWith(
    "connections.save",
    expect.objectContaining({ id, module_id: "mcp" }),
  );
  expect(
    screen.queryByRole("button", { name: "Permitir item" }),
  ).not.toBeInTheDocument();
});
test("unmounted inspector discards delayed results and never executes them", async () => {
  let resolve!: (value: unknown) => void;
  request.mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const view = render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{}}
      onSaved={vi.fn()}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Consultar lista MCP" }));
  view.unmount();
  resolve({
    tools: [
      {
        name: "late",
        alfredpetPermission: "allowed",
        alfredpetObservation: expected,
      },
    ],
  });
  await Promise.resolve();
  expect(request).toHaveBeenCalledTimes(1);
});
test("HTTP OAuth discovery submits only reviewed issuer and optional metadata URL", async () => {
  request.mockResolvedValue({
    issuer: "https://as.example/tenant",
    browserStarted: false,
    credentialsStored: false,
  });
  render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{ transport: "streamable_http" }}
      onSaved={vi.fn()}
    />,
  );
  const button = screen.getByRole("button", {
    name: "Consultar autorización MCP",
  });
  expect(button).toBeDisabled();
  fireEvent.change(screen.getByLabelText("Emisor OAuth MCP"), {
    target: { value: "https://as.example/tenant" },
  });
  fireEvent.change(screen.getByLabelText("URL de metadatos OAuth MCP"), {
    target: { value: "https://metadata.example/prm" },
  });
  expect(request).not.toHaveBeenCalled();
  fireEvent.click(button);
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith("tools.execute", {
      tool: "mcp.oauth_discover",
      input: {
        connection: id,
        issuer: "https://as.example/tenant",
        metadata_url: "https://metadata.example/prm",
      },
    }),
  );
  await screen.findByText(/Respuesta del servidor/);
  expect(request).toHaveBeenCalledTimes(1);
  expect(
    screen.queryByRole("button", { name: /Iniciar sesión/ }),
  ).not.toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("Emisor OAuth MCP"), {
    target: { value: "https://other.example" },
  });
  expect(screen.queryByText(/Respuesta del servidor/)).not.toBeInTheDocument();
});
test("stdio has no HTTP OAuth control and omitted metadata stays absent", async () => {
  request.mockResolvedValue({ issuer: "https://as.example" });
  const view = render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{ transport: "stdio" }}
      onSaved={vi.fn()}
    />,
  );
  expect(screen.queryByLabelText("Emisor OAuth MCP")).not.toBeInTheDocument();
  view.unmount();
  render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{ transport: "streamable_http" }}
      onSaved={vi.fn()}
    />,
  );
  fireEvent.change(screen.getByLabelText("Emisor OAuth MCP"), {
    target: { value: "https://as.example" },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Consultar autorización MCP" }),
  );
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith("tools.execute", {
      tool: "mcp.oauth_discover",
      input: { connection: id, issuer: "https://as.example" },
    }),
  );
});

test("OAuth registration uses an owned discovery and explicit client and scope choices", async () => {
  const discovery = {
    issuer: "https://as.example/tenant",
    requested_scopes: ["files:read"],
    alfredpetObservation: expected,
  };
  request.mockImplementation(async (_: string, payload: { tool: string }) =>
    payload.tool === "mcp.oauth_discover"
      ? discovery
      : { registrationPerformed: false, browserStarted: false },
  );
  render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{ transport: "streamable_http" }}
      onSaved={vi.fn()}
    />,
  );
  expect(
    screen.queryByRole("button", { name: "Revisar registro MCP" }),
  ).not.toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("Emisor OAuth MCP"), {
    target: { value: discovery.issuer },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Consultar autorización MCP" }),
  );
  await screen.findByRole("button", { name: "Revisar registro MCP" });
  fireEvent.change(screen.getByLabelText("Registro del cliente MCP"), {
    target: { value: "pre_registered" },
  });
  fireEvent.change(screen.getByLabelText("Identificador del cliente MCP"), {
    target: { value: "owned-client" },
  });
  fireEvent.change(screen.getByLabelText("Permisos OAuth MCP"), {
    target: { value: "files:read\nfiles:write" },
  });
  expect(request).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Revisar registro MCP" }));
  await waitFor(() =>
    expect(request).toHaveBeenLastCalledWith("tools.execute", {
      tool: "mcp.oauth_registration",
      input: {
        connection: id,
        issuer: discovery.issuer,
        expected,
        client_auth: "none",
        requested_scopes: ["files:read", "files:write"],
        preregistered: {
          client_id: "owned-client",
          redirect_uri: "http://127.0.0.1/oauth/mcp/callback",
        },
        allow_legacy_dynamic: false,
      },
    }),
  );
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Revisar registro MCP" }),
    ).toBeEnabled(),
  );
  fireEvent.change(screen.getByLabelText("Registro del cliente MCP"), {
    target: { value: "legacy_dynamic" },
  });
  fireEvent.click(screen.getByLabelText("Usar permisos predeterminados MCP"));
  fireEvent.click(screen.getByRole("button", { name: "Revisar registro MCP" }));
  await waitFor(() =>
    expect(request).toHaveBeenLastCalledWith("tools.execute", {
      tool: "mcp.oauth_registration",
      input: {
        connection: id,
        issuer: discovery.issuer,
        expected,
        client_auth: "none",
        requested_scopes: null,
        allow_legacy_dynamic: true,
      },
    }),
  );
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Revisar registro MCP" }),
    ).toBeEnabled(),
  );
  expect(
    screen.queryByRole("button", { name: /Iniciar sesión/ }),
  ).not.toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("Emisor OAuth MCP"), {
    target: { value: "https://other.example" },
  });
  expect(
    screen.queryByRole("button", { name: "Revisar registro MCP" }),
  ).not.toBeInTheDocument();
});

test("a stale pending discovery cannot populate registration after profile replacement", async () => {
  let finish: (value: unknown) => void = () => {
    throw new Error("request not started");
  };
  request.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const view = render(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{ transport: "streamable_http", endpoint: "/first" }}
      onSaved={vi.fn()}
    />,
  );
  fireEvent.change(screen.getByLabelText("Emisor OAuth MCP"), {
    target: { value: "https://as.example" },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Consultar autorización MCP" }),
  );
  expect(request).toHaveBeenCalledTimes(1);
  view.rerender(
    <McpInspector
      connection={id}
      name="Owned"
      profile={{ transport: "streamable_http", endpoint: "/second" }}
      onSaved={vi.fn()}
    />,
  );
  await act(async () => {
    finish({
      issuer: "https://as.example",
      alfredpetObservation: expected,
      requested_scopes: ["files:read"],
    });
  });
  expect(
    screen.queryByRole("button", { name: "Revisar registro MCP" }),
  ).not.toBeInTheDocument();
  expect(screen.queryByText(/Respuesta del servidor/)).not.toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Consultar autorización MCP" }),
  ).toBeEnabled();
});
