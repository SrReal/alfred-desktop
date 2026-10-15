import { createHash } from "node:crypto";

// This function is serialized into the page by Playwright. It reads metadata,
// never text input values, hidden fields or arbitrary caller-supplied JavaScript.
function describe(nodes) {
  function bounded(value, limit) {
    let text = "";
    let count = 0;
    for (const character of value) {
      if (count++ === limit) return { text, truncated: true };
      const code = character.charCodeAt(0);
      text +=
        character.length === 1 && code >= 0xd800 && code <= 0xdfff
          ? "\uFFFD"
          : character;
    }
    return { text, truncated: false };
  }

  function labelText(root) {
    const excluded =
      "input,textarea,select,script,style,template,noscript,[contenteditable],[hidden],[aria-hidden='true']";
    let text = "";
    let chars = 0;
    let current = root.firstChild;
    let visited = 0;
    while (current && visited++ < 500 && chars < 300) {
      const skip = current.nodeType === 1 && current.matches(excluded);
      if (current.nodeType === 3) {
        const part = bounded(current.data, 301)
          .text.replace(/\s+/g, " ")
          .trim();
        if (part) {
          chars += [...part].length + (text ? 1 : 0);
          text += (text ? " " : "") + part;
        }
      }
      if (!skip && current.firstChild) current = current.firstChild;
      else {
        while (current !== root && !current.nextSibling)
          current = current.parentNode;
        current = current === root ? null : current.nextSibling;
      }
    }
    return bounded(text, 300).text;
  }
  function path(node) {
    const parts = [];
    for (
      let element = node;
      element && parts.length < 32;
      element = element.parentElement
    ) {
      const tag = element.tagName.toLowerCase();
      if (
        element.id &&
        element.id.length <= 300 &&
        element.ownerDocument.querySelectorAll(`#${CSS.escape(element.id)}`)
          .length === 1
      ) {
        parts.unshift(`#${CSS.escape(element.id)}`);
        break;
      }
      const siblings = element.parentElement
        ? [...element.parentElement.children].filter(
            (sibling) => sibling.tagName === element.tagName,
          )
        : [element];
      parts.unshift(`${tag}:nth-of-type(${siblings.indexOf(element) + 1})`);
    }
    return bounded(parts.join(" > "), 2000).text;
  }
  return nodes.slice(0, 150).map((node) => {
    const label = bounded(
      (
        node.getAttribute("aria-label") ||
        [...(node.labels ?? [])].map(labelText).join(" ") ||
        node.getAttribute("title") ||
        ""
      ).trim(),
      300,
    ).text;
    const type = (node.getAttribute("type") || "").toLowerCase();
    const autocomplete = (
      node.getAttribute("autocomplete") || ""
    ).toLowerCase();
    const sensitive =
      type === "password" ||
      type === "hidden" ||
      /password|passwd|secret|token|authorization|one-time|credit|cc-|otp/i.test(
        `${label} ${node.name ?? ""} ${node.id} ${autocomplete}`,
      );
    const action = node.form ? new URL(node.form.action) : null;
    const actionSensitive =
      action &&
      (action.username ||
        action.password ||
        action.href.length > 8192 ||
        /access_token|id_token|password|secret|authorization|api.?key|csrf|session|(?:^|[&#])(?:sid|code)=/i.test(
          action.hash,
        ) ||
        [...action.searchParams.keys()].some((key) =>
          /password|passwd|secret|token|authorization|api.?key|csrf|session|^sid$|^code$/i.test(
            key,
          ),
        ));
    return {
      selector: path(node),
      tag: node.tagName.toLowerCase(),
      type,
      id: bounded(node.id, 300).text,
      name: bounded(node.name ?? "", 300).text,
      label,
      disabled: !!node.disabled,
      required: !!node.required,
      multiple: !!node.multiple,
      sensitive,
      checked:
        node.tagName === "INPUT" &&
        ["checkbox", "radio"].includes(type) &&
        !sensitive
          ? node.checked
          : null,
      form: node.form
        ? {
            action: actionSensitive ? "[REDACTED]" : action.href,
            method: node.form.method,
          }
        : null,
      options:
        node.tagName === "SELECT" && !sensitive
          ? [...node.options].slice(0, 50).map((option) => ({
              label: bounded(option.label, 300).text,
              disabled: option.disabled,
              selected: option.selected,
              truncated: bounded(option.label, 300).truncated,
            }))
          : [],
      options_truncated: node.tagName === "SELECT" && node.options.length > 50,
    };
  });
}
function fingerprint(control) {
  return createHash("sha256").update(JSON.stringify(control)).digest("hex");
}
function pageIdentity(page, input) {
  if (typeof input.url !== "string" || input.url !== page.url())
    throw new Error("BROWSER_PAGE_CHANGED");
  if (input.url.length > 8192) throw new Error("BROWSER_URL_LIMIT");
  const url = new URL(page.url());
  if (
    !["http:", "https:"].includes(url.protocol) ||
    url.username ||
    url.password
  )
    throw new Error("BROWSER_URL_DENIED");
  if (
    [...url.searchParams.keys()].some((key) =>
      /password|passwd|secret|token|authorization|api.?key|csrf|session|^sid$|^code$/i.test(
        key,
      ),
    ) ||
    /access_token|id_token|password|secret|authorization|api.?key|csrf|session|(?:^|[&#])(?:sid|code)=/i.test(
      url.hash,
    )
  )
    throw new Error("BROWSER_URL_SECRET_DENIED");
}
async function target(page, input) {
  pageIdentity(page, input);
  if (
    typeof input.selector !== "string" ||
    !input.selector ||
    [...input.selector].length > 2000
  )
    throw new Error("BROWSER_SELECTOR");
  const locator = page.locator(input.selector);
  if ((await locator.count()) !== 1)
    throw new Error("BROWSER_TARGET_AMBIGUOUS_OR_MISSING");
  const control = (await locator.evaluateAll(describe))[0];
  if (control.sensitive) throw new Error("SENSITIVE_FORM_CONTROL_DENIED");
  if (control.disabled || !(await locator.isVisible()))
    throw new Error("BROWSER_TARGET_DISABLED_OR_HIDDEN");
  if (input.body?.fingerprint !== fingerprint(control))
    throw new Error("BROWSER_CONTROL_CHANGED: inspect forms before retrying");
  return { locator, control };
}
export async function formOperation(page, operation, input) {
  if (operation === "browser.forms") {
    pageIdentity(page, { url: page.url() });
    const all = page.locator("input,select,textarea,button");
    const count = await all.count();
    const controls = await all.evaluateAll(describe);
    return {
      url: page.url(),
      title: await page.title(),
      controls: controls.map((control) => ({
        ...control,
        fingerprint: fingerprint(control),
      })),
      truncated: count > 150,
      text_input_values_read: false,
    };
  }
  if (operation === "browser.extract") {
    pageIdentity(page, input);
    const locator = page.locator(input.selector ?? "body");
    const count = await locator.count();
    if (count !== 1) throw new Error("BROWSER_TARGET_AMBIGUOUS_OR_MISSING");
    const limit = input.body?.max_chars ?? 16000;
    if (!Number.isInteger(limit) || limit < 1 || limit > 64000)
      throw new Error("BROWSER_EXTRACT_LIMIT");
    const data = await locator.evaluate((node, limit) => {
      function bounded(value, limit) {
        let text = "";
        let count = 0;
        for (const character of value) {
          if (count++ === limit) return { text, truncated: true };
          const code = character.charCodeAt(0);
          text +=
            character.length === 1 && code >= 0xd800 && code <= 0xdfff
              ? "\uFFFD"
              : character;
        }
        return { text, truncated: false };
      }
      const excluded =
        "input,textarea,select,script,style,template,noscript,[contenteditable]";
      if (node.closest(excluded))
        throw new Error("BROWSER_EXTRACT_FORM_VALUES_DENIED");
      function hidden(element) {
        const style = getComputedStyle(element);
        return (
          element.hidden ||
          element.getAttribute("aria-hidden") === "true" ||
          style.display === "none" ||
          ["hidden", "collapse"].includes(style.visibility)
        );
      }
      function readable(root) {
        let ancestor = root;
        let depth = 0;
        while (ancestor && depth++ < 32) {
          if (ancestor.matches(excluded) || hidden(ancestor)) return false;
          ancestor = ancestor.parentElement;
        }
        return !ancestor;
      }
      function textOnly(root, maximum) {
        if (!readable(root)) return { text: "", truncated: false };
        let text = "";
        let chars = 0;
        let current = root.firstChild;
        let visited = 0;
        let shortened = false;
        while (current && visited++ < 10000 && chars <= maximum) {
          const skip =
            current.nodeType === 1 &&
            (current.matches(excluded) || hidden(current));
          if (current.nodeType === 3) {
            const raw = current.data;
            const prefix = bounded(raw, maximum + 1);
            shortened ||= prefix.truncated;
            const part = prefix.text.replace(/\s+/g, " ").trim();
            if (part) {
              chars += [...part].length + (text ? 1 : 0);
              text += (text ? " " : "") + part;
            }
          }
          if (!skip && current.firstChild) current = current.firstChild;
          else {
            while (current !== root && !current.nextSibling)
              current = current.parentNode;
            current = current === root ? null : current.nextSibling;
          }
        }
        return {
          text: bounded(text, maximum).text,
          truncated: shortened || !!current || bounded(text, maximum).truncated,
        };
      }
      const content = textOnly(node, limit);
      const links = [...node.querySelectorAll("a[href]")]
        .slice(0, 100)
        .filter((link) => {
          try {
            const url = new URL(link.href);
            return (
              readable(link) &&
              ["http:", "https:"].includes(url.protocol) &&
              !url.username &&
              !url.password &&
              url.href.length <= 8192 &&
              ![...url.searchParams.keys()].some((key) =>
                /password|passwd|secret|token|authorization|api.?key|csrf|session|^sid$|^code$/i.test(
                  key,
                ),
              ) &&
              !/access_token|id_token|password|secret|authorization|api.?key|csrf|session|(?:^|[&#])(?:sid|code)=/i.test(
                url.hash,
              )
            );
          } catch {
            return false;
          }
        })
        .map((link) => ({
          text: textOnly(link, 300).text,
          url: link.href,
        }));
      return {
        ...content,
        links,
      };
    }, limit);
    return {
      url: page.url(),
      title: await page.title(),
      ...data,
      untrusted_content: true,
    };
  }
  const { locator, control } = await target(page, input);
  if (operation === "browser.select") {
    const labels = input.body?.labels;
    if (
      control.tag !== "select" ||
      !Array.isArray(labels) ||
      !labels.length ||
      labels.length > 20 ||
      labels.some(
        (label) =>
          typeof label !== "string" || !label || [...label].length > 300,
      ) ||
      new Set(labels).size !== labels.length ||
      (!control.multiple && labels.length !== 1)
    )
      throw new Error("BROWSER_SELECT_INPUT");
    if (control.options_truncated) throw new Error("BROWSER_OPTIONS_LIMIT");
    for (const label of labels) {
      const options = control.options.filter(
        (option) => option.label === label,
      );
      if (options.length !== 1 || options[0].disabled || options[0].truncated)
        throw new Error("BROWSER_OPTION_AMBIGUOUS_MISSING_OR_DISABLED");
    }
    await locator.selectOption(
      labels.map((label) => ({ label })),
      { timeout: 15000 },
    );
    const actual = await locator.evaluate((node) =>
      [...node.selectedOptions].map((option) => option.label),
    );
    if (
      actual.length !== labels.length ||
      labels.some((label) => !actual.includes(label))
    )
      throw new Error("BROWSER_SELECT_READBACK_FAILED");
    pageIdentity(page, { url: page.url() });
    return { selected_labels: actual, verified: true, url: page.url() };
  }
  if (operation === "browser.check") {
    if (
      control.tag !== "input" ||
      !["checkbox", "radio"].includes(control.type) ||
      typeof input.body?.checked !== "boolean" ||
      (control.type === "radio" && !input.body.checked)
    )
      throw new Error("BROWSER_CHECK_INPUT");
    await locator.setChecked(input.body.checked, { timeout: 15000 });
    const actual = await locator.isChecked();
    if (actual !== input.body.checked)
      throw new Error("BROWSER_CHECK_READBACK_FAILED");
    pageIdentity(page, { url: page.url() });
    return { checked: actual, verified: true, url: page.url() };
  }
  throw new Error("BROWSER_FORM_OPERATION");
}
