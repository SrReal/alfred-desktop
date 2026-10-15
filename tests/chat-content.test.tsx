import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ChatMessageContent } from "../src/ui/ChatMessageContent";
import { request } from "../src/api";
vi.mock("../src/api", () => ({
  request: vi.fn(),
  on: vi.fn(async () => () => {}),
}));
beforeEach(() => {
  vi.mocked(request).mockReset();
});

test("renders headings, emphasis, lists, tables, code and automatic links", () => {
  const { container } = render(
    <ChatMessageContent
      text={
        "## Informe\n\n**Listo** y *revisado*\n\n- Uno\n- Dos\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n```js\nconst x = 1;\n```\n\nhttps://example.com"
      }
    />,
  );
  expect(screen.getByRole("heading", { name: "Informe" })).toBeTruthy();
  expect(container.querySelector("strong")?.textContent).toBe("Listo");
  expect(screen.getAllByRole("listitem")).toHaveLength(2);
  expect(screen.getByRole("table")).toBeTruthy();
  expect(container.querySelector("pre code")?.textContent).toContain(
    "const x = 1;",
  );
  expect(screen.getByRole("link").getAttribute("href")).toBe(
    "https://example.com/",
  );
});
test("opens explicit clicks through the desktop handler and reports errors", async () => {
  vi.mocked(request).mockResolvedValue({});
  render(<ChatMessageContent text="[Abrir](https://example.com/docs)" />);
  expect(request).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("link"));
  expect(request).toHaveBeenCalledWith("chat.open_link", {
    url: "https://example.com/docs",
  });
  vi.mocked(request).mockRejectedValue(new Error("Sin navegador"));
  fireEvent.click(screen.getByRole("link"));
  await screen.findByRole("alert");
  expect(screen.getByRole("alert").textContent).toContain("Sin navegador");
});
test("does not render raw HTML or active/local URL schemes", () => {
  const { container } = render(
    <ChatMessageContent
      text={
        '<script>alert(1)</script>\n\n<iframe src="https://example.com"></iframe>\n\n[A](javascript:alert%281%29) [B](file:///C:/test.exe) [C](ms-settings:privacy) [D](data:text/html,x) ![E](file:///C:/private.png)'
      }
    />,
  );
  expect(container.querySelector("script,iframe,a,img")).toBeNull();
  expect(request).not.toHaveBeenCalled();
});
test("shows HTTPS images with a fallback for failure; blocks data and HTTP images", () => {
  const { container } = render(
    <ChatMessageContent text="![Plano](https://example.com/plan.png) ![Insegura](http://example.com/x.png) ![Datos](data:image/png;base64,AA==)" />,
  );
  const image = screen.getByAltText("Plano");
  expect(image.getAttribute("referrerpolicy")).toBe("no-referrer");
  expect(container.querySelectorAll("img")).toHaveLength(1);
  fireEvent.error(image);
  expect(container.querySelector("img")).toBeNull();
  expect(screen.getByText(/Plano · imagen no disponible/)).toBeTruthy();
});
test("renders registered asset previews and preserves streaming updates", async () => {
  const id = "00000000-0000-4000-8000-000000000001";
  vi.mocked(request).mockResolvedValue({
    thumbnail_data_url: "data:image/png;base64,AA==",
  });
  const view = render(
    <ChatMessageContent
      text={`![Captura](alfred-asset:${id})\n\n**Analizando`}
    />,
  );
  await waitFor(() => expect(screen.getByAltText("Captura")).toBeTruthy());
  expect(request).toHaveBeenCalledWith("assets.get", { id });
  view.rerender(<ChatMessageContent text="**Terminado**" />);
  expect(screen.getByText("Terminado").tagName).toBe("STRONG");
});

test("copies only the exact code text on click, including indentation and streaming changes", async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  const view = render(
    <ChatMessageContent text={"```js\n  const x = 1;\n```"} />,
  );
  expect(writeText).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Copiar código" }));
  await waitFor(() =>
    expect(writeText).toHaveBeenCalledWith("  const x = 1;\n"),
  );
  view.rerender(<ChatMessageContent text={"```js\n  const x = 2;\n```"} />);
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Copiar código" })).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Copiar código" }));
  await waitFor(() =>
    expect(writeText).toHaveBeenLastCalledWith("  const x = 2;\n"),
  );
  writeText.mockRejectedValue(new Error("Denied"));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Copiar código" })).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Copiar código" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Ctrl+C");
});
