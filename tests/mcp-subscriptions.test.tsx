import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { McpSubscriptions } from "../src/ui/McpSubscriptions";
const request = vi.hoisted(() => vi.fn());
vi.mock("../src/api", () => ({ request }));
const id = "11111111-1111-4111-8111-111111111111";
const streamId = "22222222-2222-4222-8222-222222222222";
const profile = { permissions: { resources: ["owned:///one"] } };
const stream = {
  connection: id,
  stream_id: streamId,
  status: "active",
  honored: { toolsListChanged: true },
  received: 0,
  lost: 0,
  queued: 0,
};
beforeEach(() => {
  request.mockReset();
});
test("explicit filter start shows honored subset and manual poll invalidates observations without network replay", async () => {
  const changed = vi.fn();
  request.mockImplementation(async (_, v) =>
    v.tool === "mcp.listen_poll"
      ? { ...stream, received: 1, events: [{ type: "tools_changed" }] }
      : v.tool === "mcp.listen_stop"
        ? { ...stream, status: "closed" }
        : stream,
  );
  render(
    <McpSubscriptions connection={id} profile={profile} onChanged={changed} />,
  );
  expect(request).not.toHaveBeenCalled();
  const start = screen.getByRole("button", { name: "Iniciar suscripción MCP" });
  expect(start).toBeDisabled();
  fireEvent.click(screen.getByLabelText("Cambios de herramientas MCP"));
  fireEvent.click(screen.getByLabelText("Avisos de owned:///one"));
  fireEvent.click(start);
  await waitFor(() =>
    expect(request).toHaveBeenCalledWith("tools.execute", {
      tool: "mcp.listen",
      input: {
        connection: id,
        notifications: {
          toolsListChanged: true,
          resourceSubscriptions: ["owned:///one"],
        },
      },
    }),
  );
  const poll = await screen.findByRole("button", { name: "Leer avisos MCP" });
  await waitFor(() => expect(poll).toBeEnabled());
  fireEvent.click(poll);
  await screen.findByLabelText("Avisos MCP recibidos");
  expect(changed).toHaveBeenCalledOnce();
  fireEvent.click(
    screen.getByRole("button", { name: "Detener suscripción MCP" }),
  );
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Leer avisos MCP" }),
    ).toBeDisabled(),
  );
  expect(
    request.mock.calls.filter(([, v]) => v.tool === "mcp.listen"),
  ).toHaveLength(1);
});
test("profile change ignores late ACK and clears old filters and events", async () => {
  let resolve!: (v: unknown) => void;
  request.mockImplementation(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const view = render(
    <McpSubscriptions connection={id} profile={profile} onChanged={vi.fn()} />,
  );
  fireEvent.click(screen.getByLabelText("Cambios de herramientas MCP"));
  fireEvent.click(
    screen.getByRole("button", { name: "Iniciar suscripción MCP" }),
  );
  view.rerender(
    <McpSubscriptions
      connection={streamId}
      profile={profile}
      onChanged={vi.fn()}
    />,
  );
  resolve(stream);
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Iniciar suscripción MCP" }),
    ).toBeDisabled(),
  );
  expect(
    screen.queryByRole("button", { name: "Leer avisos MCP" }),
  ).not.toBeInTheDocument();
});
test("poll rejection discards events and requires fresh catalog review", async () => {
  const changed = vi.fn();
  request.mockImplementation(async (_, v) => {
    if (v.tool === "mcp.listen_poll") throw new Error("MCP_REVOKED");
    return stream;
  });
  render(
    <McpSubscriptions connection={id} profile={profile} onChanged={changed} />,
  );
  fireEvent.click(screen.getByLabelText("Cambios de herramientas MCP"));
  fireEvent.click(
    screen.getByRole("button", { name: "Iniciar suscripción MCP" }),
  );
  const poll = await screen.findByRole("button", { name: "Leer avisos MCP" });
  await waitFor(() => expect(poll).toBeEnabled());
  fireEvent.click(poll);
  expect(await screen.findByRole("alert")).toHaveTextContent("MCP_REVOKED");
  expect(changed).toHaveBeenCalledOnce();
  expect(
    screen.queryByLabelText("Avisos MCP recibidos"),
  ).not.toBeInTheDocument();
});
