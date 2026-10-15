import { chromium } from "playwright-core";
import { createInterface } from "node:readline";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { formOperation } from "./forms.mjs";
const edge = [
  process.env.ALFREDPET_BROWSER,
  join(
    process.env["ProgramFiles(x86)"] ?? "",
    "Microsoft/Edge/Application/msedge.exe",
  ),
  join(process.env.ProgramFiles ?? "", "Google/Chrome/Application/chrome.exe"),
].find((p) => p && existsSync(p));
let context;
const pages = new Map();
async function execute(req) {
  const input = req.input;
  if (!context) {
    if (!edge) throw new Error("BROWSER_NOT_INSTALLED");
    context = await chromium.launchPersistentContext(
      process.env.ALFREDPET_BROWSER_PROFILE,
      { executablePath: edge, headless: false, acceptDownloads: true },
    );
    context.setDefaultTimeout(15000);
    context.on("page", (p) => {
      pages.set(String(pages.size + 1), p);
    });
    pages.set("1", context.pages()[0] ?? (await context.newPage()));
  }
  if (req.operation === "browser.tabs")
    return [...pages].map(([id, p]) => ({ id, url: p.url() }));
  const page = pages.get(input.tab ?? "1");
  if (!page || page.isClosed()) throw new Error("TAB_NOT_FOUND");
  if (
    [
      "browser.forms",
      "browser.extract",
      "browser.select",
      "browser.check",
    ].includes(req.operation)
  )
    return formOperation(page, req.operation, input);
  switch (req.operation) {
    case "browser.open":
      await page.goto(input.url, { waitUntil: "domcontentloaded" });
      return { url: page.url(), title: await page.title() };
    case "browser.inspect": {
      const extracted = await formOperation(page, "browser.extract", {
        url: page.url(),
      });
      const forms = await formOperation(page, "browser.forms", {});
      return {
        ...extracted,
        elements: forms.controls
          .map((control) => ({
            tag: control.tag.toUpperCase(),
            text: control.label,
            id: control.id,
            name: control.name,
            type: control.type,
            sensitive: control.sensitive,
          }))
          .concat(
            extracted.links.map((link) => ({
              tag: "A",
              text: link.text,
              url: link.url,
            })),
          ),
        forms_truncated: forms.truncated,
      };
    }
    case "browser.click":
      await page.locator(input.selector).click();
      return { url: page.url() };
    case "browser.type":
      if (
        (await page.locator(input.selector).getAttribute("type")) === "password"
      )
        throw new Error("PASSWORD_DENIED");
      await page.locator(input.selector).fill(input.text);
      return { filled: true };
    case "browser.upload":
      await page.locator(input.selector).setInputFiles(input.path);
      return { uploaded: true };
    case "browser.download": {
      const waiting = page.waitForEvent("download");
      await page.locator(input.selector).click();
      const d = await waiting;
      await d.saveAs(input.output);
      return { path: input.output, suggestedName: d.suggestedFilename() };
    }
    case "browser.screenshot": {
      if (input.output !== undefined)
        throw new Error("SCREENSHOT_OUTPUT_DENIED");
      const viewport = page.viewportSize();
      if (
        !viewport ||
        viewport.width < 1 ||
        viewport.height < 1 ||
        viewport.width > 16384 ||
        viewport.height > 16384 ||
        viewport.width * viewport.height > 33554432
      )
        throw new Error("CAPTURE_PIXEL_LIMIT");
      const png = await page.screenshot({
        type: "png",
        fullPage: false,
        scale: "css",
        timeout: 15000,
      });
      if (png.length > 33554432) throw new Error("CAPTURE_BYTE_LIMIT");
      return {
        png_base64: png.toString("base64"),
        rect: { x: 0, y: 0, ...viewport },
        coordinate_space: "css",
        method: "Playwright viewport",
      };
    }
    case "browser.wait":
      await page.locator(input.selector).waitFor({ state: "visible" });
      return { ready: true };
    case "browser.submit":
      await page.locator(input.selector).click();
      return { submitted: true, url: page.url() };
    default:
      throw new Error("BROWSER_OPERATION_UNKNOWN");
  }
}
for await (const line of createInterface({ input: process.stdin })) {
  let req;
  try {
    req = JSON.parse(line);
    const data = await execute(req);
    process.stdout.write(
      JSON.stringify({ id: req.id, ok: true, data }, (_key, value) =>
        typeof value === "string" ? value.toWellFormed() : value,
      ) + "\n",
    );
  } catch (e) {
    process.stdout.write(
      JSON.stringify(
        {
          id: req?.id,
          ok: false,
          error:
            e instanceof Error ? e.message.toWellFormed() : "BROWSER_ERROR",
        },
        (_key, value) =>
          typeof value === "string" ? value.toWellFormed() : value,
      ) + "\n",
    );
  }
}
await context?.close();
