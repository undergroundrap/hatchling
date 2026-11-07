// The page's snapshot, read in the controller's isolated world: what the
// page shows, and the elements it can be acted on through. The elements
// are kept in this world's own global, which the page's scripts can't
// see, for the actions that follow. Bounded here, and again by the
// controller. A password field's value is never read. Anything cut, a
// label too, sets `truncated`: hiding a value shortens a field, so a later
// pass can't tell a label was cut from its length alone.
(() => {
  const MAX_TEXT = 12288;
  const MAX_ELEMENTS = 150;
  const MAX_LABEL = 120;
  let cut = false;
  const clip = (value, max) => {
    const text = String(value || "").replace(/\s+/g, " ").trim();
    if (text.length > max) {
      cut = true;
      return text.slice(0, max);
    }
    return text;
  };
  const visible = (element) => {
    if (element.getClientRects().length === 0) {
      return false;
    }
    const style = getComputedStyle(element);
    return style.visibility !== "hidden" && style.display !== "none";
  };
  const INPUT_ROLES = ["checkbox", "radio", "file", "range", "color", "date"];
  const ARIA_ROLES = ["button", "link", "checkbox", "radio", "switch", "tab", "menuitem", "option", "combobox", "listbox", "textbox", "searchbox", "slider"];
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
    // A page writes its own role, so only a word from this list passes:
    // anything else, a secret it read included, is just an element.
    const role = (element.getAttribute("role") || "").trim().toLowerCase();
    if (role) {
      return ARIA_ROLES.includes(role) ? role : "element";
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
  // Whether it's really a link, from the element and not from anything the
  // page says of it: a page can give anything a role, but only an <a> has
  // these. They're read here, in the isolated world, where the page's own
  // scripts can't stand in for them.
  const realLink = (element) =>
    element instanceof HTMLAnchorElement &&
    (element.protocol === "http:" || element.protocol === "https:");
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
  const url = clip(location.href, MAX_LABEL);
  const title = clip(document.title, MAX_LABEL);
  const elements = kept.map((element, id) => {
    const role = roleOf(element);
    return { id, role, name: nameOf(element, role), link: realLink(element) };
  });
  return {
    url,
    title,
    text: text.length > MAX_TEXT ? text.slice(0, MAX_TEXT) : text,
    elements,
    truncated: cut || found.length > MAX_ELEMENTS || text.length > MAX_TEXT,
  };
})()
