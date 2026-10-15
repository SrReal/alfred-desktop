import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { request } from "../src/api";
import { Watches } from "../src/ui/App";
vi.mock("../src/api", () => ({
  request: vi.fn(),
  on: vi.fn(async () => () => {}),
  desktop: false,
  show: vi.fn(),
}));
const call = vi.mocked(request);
beforeEach(() => {
  call.mockReset();
  call.mockResolvedValue([]);
});
describe("Explicit page/UI watches", () => {
  it("passes an owned tab, exact URL and selector after a user creates a page watch", async () => {
    render(<Watches />);
    await waitFor(() => expect(call).toHaveBeenCalledWith("watches.list", {}));
    fireEvent.change(screen.getByLabelText("Tipo de vigilancia"), {
      target: { value: "page" },
    });
    fireEvent.change(screen.getByLabelText("Destino de vigilancia"), {
      target: { value: "https://example.org/owned" },
    });
    fireEvent.change(screen.getByLabelText("Opciones de vigilancia"), {
      target: {
        value:
          '{"tab":"owned","page_selector":"#content","condition":"changed"}',
      },
    });
    fireEvent.click(screen.getByText("Crear vigilancia"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("tools.execute", {
        tool: "monitoring.create",
        input: {
          kind: "page",
          path: "https://example.org/owned",
          recursive: false,
          body: {
            tab: "owned",
            page_selector: "#content",
            condition: "changed",
          },
        },
      }),
    );
    expect(
      call.mock.calls.filter(([op]) => op === "tools.execute"),
    ).toHaveLength(1);
  });
  it("requires valid JSON before sending a selected UI target", async () => {
    render(<Watches />);
    fireEvent.change(screen.getByLabelText("Tipo de vigilancia"), {
      target: { value: "ui_element" },
    });
    fireEvent.change(screen.getByLabelText("Destino de vigilancia"), {
      target: { value: "123" },
    });
    fireEvent.change(screen.getByLabelText("Opciones de vigilancia"), {
      target: { value: "broken json" },
    });
    fireEvent.click(screen.getByText("Crear vigilancia"));
    expect(await screen.findByRole("alert")).toBeVisible();
    expect(
      call.mock.calls.filter(([op]) => op === "tools.execute"),
    ).toHaveLength(0);
    fireEvent.change(screen.getByLabelText("Opciones de vigilancia"), {
      target: {
        value:
          '{"process_id":44,"selector":{"automation_id":"owned"},"condition":"exists"}',
      },
    });
    fireEvent.click(screen.getByText("Crear vigilancia"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("tools.execute", {
        tool: "monitoring.create",
        input: {
          kind: "ui_element",
          path: "123",
          recursive: false,
          body: {
            process_id: 44,
            selector: { automation_id: "owned" },
            condition: "exists",
          },
        },
      }),
    );
  });
});
