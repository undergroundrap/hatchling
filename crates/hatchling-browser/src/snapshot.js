// The page's snapshot, read in the controller's isolated world: what the
// page shows, and the elements it can be acted on through. The elements
// are kept in this world's own global, which the page's scripts can't
// see, for the actions that follow. Bounded here, and again by the
// controller. A password field's value is never read.
(() => {
  const MAX_TEXT = 12288;
  const MAX_ELEMENTS = 150;
  const MAX_LABEL = 120;
  const clip = (value, max) => {
    const text = String(value || "").replace(/\s+/g, " ").trim();
    return text.length > max ? text.slice(0, max) : text;
  };
  const visible = (element) => {
    if (element.getClientRects().length === 0) {
      return false;
    }
    const style = getComputedStyle(element);
    return style.visibility !== "hidden" && style.display !== "none";
  };
  const INPUT_ROLES = ["checkbox", "radio", "file", "range", "color", "date"];
  const roleOf = (element) => {
    const tag = element.tagName.toLowerCase();
    if (tag === "input") {
      const type = (element.type || "text").toLowerCase();
      if (type === "password") {
        return "password";
      }
      if (type === "submit" || type === "reset" || type === "button" || type === "image") {
        return "button";
      }
      return INPUT_ROLES.includes(type) ? type : "textbox";
    }
    const role = element.getAttribute("role");
    if (role) {
      return clip(role, 30);
    }
    if (tag === "a") {
      return "link";
    }
    if (tag === "select") {
      return "list";
    }
    if (tag === "textarea" || element.isContentEditable) {
      return "textbox";
    }
    return "button";
  };
  const labelOf = (element) =>
    element.labels && element.labels.length > 0 ? element.labels[0].innerText : "";
  const nameOf = (element, role) => {
    const given = element.getAttribute("aria-label") || labelOf(element);
    if (role === "password") {
      return clip(given || element.placeholder || element.name || "password", MAX_LABEL);
    }
    if (element.tagName === "SELECT") {
      const options = Array.from(element.options).map((option) => option.text.trim());
      return clip(`${given || element.name || ""} [${options.join(" | ")}]`, MAX_LABEL);
    }
    const typed =
      element.tagName === "INPUT" || element.tagName === "TEXTAREA"
        ? element.value || element.placeholder || ""
        : "";
    return clip(
      given ||
        element.innerText ||
        typed ||
        element.getAttribute("title") ||
        element.getAttribute("alt") ||
        element.name ||
        "",
      MAX_LABEL,
    );
  };
  const found = Array.from(
    document.querySelectorAll(
      'a[href], button, input:not([type="hidden"]), select, textarea, [role="button"], ' +
        '[role="link"], [role="checkbox"], [role="tab"], [role="menuitem"], ' +
        '[contenteditable=""], [contenteditable="true"], [onclick]',
    ),
  ).filter(visible);
  const kept = found.slice(0, MAX_ELEMENTS);
  globalThis.__hatchling = kept;
  const text = document.body ? document.body.innerText : "";
  return {
    url: clip(location.href, MAX_LABEL),
    title: clip(document.title, MAX_LABEL),
    text: text.length > MAX_TEXT ? text.slice(0, MAX_TEXT) : text,
    elements: kept.map((element, id) => {
      const role = roleOf(element);
      return { id, role, name: nameOf(element, role) };
    }),
    truncated: found.length > MAX_ELEMENTS || text.length > MAX_TEXT,
  };
})()
