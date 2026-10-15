import { render, screen, fireEvent, act } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { WindowRecovery } from "../src/ui/WindowRecovery";
const mock = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => mock);
const row = {
  id: "window-1",
  description: "window.move · 1 ventanas",
  state: "available",
};
beforeEach(() => {
  mock.request.mockReset();
  mock.on.mockReset();
  mock.on.mockResolvedValue(() => {});
  mock.request.mockImplementation(async (_op, input) =>
    input.tool === "window.undo_history" ? [row] : { state: "undone" },
  );
});
test("window recovery requires an explicit action and uses the window undo tool", async () => {
  render(<WindowRecovery />);
  await screen.findByText(row.description);
  expect(mock.request).not.toHaveBeenCalledWith(
    "tools.execute",
    expect.objectContaining({ tool: "window.undo" }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Solicitar deshacer" }));
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Posición y estado anteriores restaurados.",
  );
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "window.undo",
    input: { id: row.id },
  });
});
test("blocked windows preserve conflicts and only retry by explicit revalidation", async () => {
  let fail = true;
  let activity: (() => void) | undefined;
  mock.on.mockImplementation(async (_event, callback) => {
    activity = callback;
    return () => {};
  });
  mock.request.mockImplementation(async (_op, input) => {
    if (input.tool === "window.undo_history")
      return [
        { ...row, state: "blocked", error: "WINDOW_CHANGED_AFTER_ACTION" },
      ];
    if (fail)
      throw new Error("WINDOW_CHANGED_AFTER_ACTION: preserve later changes");
    return { reconciled: true };
  });
  render(<WindowRecovery />);
  const button = await screen.findByRole("button", {
    name: "Revalidar y solicitar deshacer",
  });
  fireEvent.click(button);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "preserve later changes",
  );
  await act(async () => {
    activity?.();
  });
  expect(screen.getByRole("alert")).toHaveTextContent("preserve later changes");
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  expect(screen.getByText(row.description)).toBeInTheDocument();
  fail = false;
  fireEvent.click(button);
  expect(await screen.findByRole("status")).toHaveTextContent(
    "Estado de ventanas verificado.",
  );
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "window.undo",
    input: { id: row.id, revalidate: true },
  });
});
