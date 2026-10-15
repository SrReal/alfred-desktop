import {
  act,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import {
  ChatProgress,
  mergeProgress,
  runStatus,
  useChatProgress,
  type ChatRun,
} from "../src/ui/ChatProgress";
import { ChatTranscript } from "../src/ui/ChatTranscript";
const api = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => api);
const run: ChatRun = {
  id: "run",
  conversation_id: "chat",
  message_id: "user",
  revision: 1,
  status: "running",
  created_at: "2026-10-13T10:00:00Z",
  updated_at: "2026-10-13T10:00:01Z",
  plan: [{ id: "p", title: "Consultar información", status: "in_progress" }],
  actions: [
    { id: "a", tool: "skills.list", status: "completed" },
    { id: "b", tool: "files.copy", status: "waiting_permission" },
  ],
};
beforeEach(() => {
  vi.clearAllMocks();
  api.request.mockResolvedValue([]);
  api.on.mockResolvedValue(() => {});
});
test("live progress shows actual waits, has a collapsible plan and opens existing request UI", async () => {
  render(<ChatProgress run={run} />);
  expect(
    screen.getByRole("button", { name: /Plan y progreso/ }),
  ).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByText(/1\/2 acciones/)).toHaveTextContent(
    "Esperando autorización",
  );
  fireEvent.click(screen.getByRole("button", { name: "Revisar solicitud" }));
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("window.show", {
      surface: "permission",
    }),
  );
  expect(api.request).not.toHaveBeenCalledWith(
    "permissions.resolve",
    expect.anything(),
  );
  fireEvent.click(screen.getByRole("button", { name: /Plan y progreso/ }));
  expect(screen.queryByText("Consultar información")).not.toBeInTheDocument();
});
test("finishing never hides errors or pretends pending steps are done", () => {
  expect(
    runStatus({
      ...run,
      status: "completed",
      actions: [{ ...run.actions[0], status: "denied" }],
    }),
  ).toBe("Finalizado con incidencias");
  expect(runStatus({ ...run, status: "completed", actions: [] })).toBe(
    "Finalizado con pasos pendientes",
  );
  expect(runStatus({ ...run, status: "interrupted" })).toBe("Interrumpido");
  expect(
    runStatus({
      ...run,
      actions: [{ ...run.actions[0], status: "waiting_user" }],
    }),
  ).toBe("Esperando tu respuesta");
  render(<ChatProgress run={{ ...run, status: "completed" }} />);
  expect(screen.getByRole("button")).toHaveAttribute("aria-expanded", "false");
});
test("progress anchors to the request and survives the persisted response without duplication", () => {
  const messages = [
    {
      id: "user",
      role: "user",
      content_text: "Solicitud",
      status: "complete",
      created_at: run.created_at,
    },
    {
      id: "reply",
      role: "assistant",
      content_text: "Respuesta",
      status: "complete",
      created_at: run.updated_at,
    },
  ];
  const v = render(
    <ChatTranscript
      conversationId="chat"
      messages={[]}
      stream=""
      pendingText=""
      progress={[run]}
    />,
  );
  v.rerender(
    <ChatTranscript
      conversationId="chat"
      messages={messages}
      stream=""
      pendingText=""
      progress={[run]}
    />,
  );
  expect(
    screen.getAllByRole("region", { name: "Plan y progreso" }),
  ).toHaveLength(1);
  const children = [
    ...v.container.querySelector(".transcript-content")!.children,
  ];
  expect(children[0]).toHaveTextContent("Solicitud");
  expect(children[1]).toHaveTextContent("Plan y progreso");
  expect(children[2]).toHaveTextContent("Respuesta");
});
test("revisions prevent stale events and snapshots undoing progress", () => {
  const completed = { ...run, revision: 4, status: "completed" };
  expect(mergeProgress([completed], [run, run])).toEqual([completed]);
});
test("subscribe before loading, merge race and discard old conversation callbacks", async () => {
  const callbacks: ((r: ChatRun) => void)[] = [];
  const snapshots: ((r: ChatRun[]) => void)[] = [];
  api.on.mockImplementation(async (_: string, fn: (r: ChatRun) => void) => {
    callbacks.push(fn);
    return vi.fn();
  });
  api.request.mockImplementation(
    () => new Promise<ChatRun[]>((resolve) => snapshots.push(resolve)),
  );
  const v = renderHook(({ id }) => useChatProgress(id), {
    initialProps: { id: "chat" },
  });
  await waitFor(() => expect(snapshots).toHaveLength(1));
  act(() => callbacks[0]({ ...run, revision: 5, status: "completed" }));
  await act(async () => snapshots[0]([run]));
  expect(v.result.current.runs[0].status).toBe("completed");
  v.rerender({ id: "other" });
  await waitFor(() => expect(snapshots).toHaveLength(2));
  act(() => callbacks[0]({ ...run, revision: 9 }));
  await act(async () => snapshots[1]([]));
  expect(v.result.current.runs).toEqual([]);
});
