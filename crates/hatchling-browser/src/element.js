// An action's first step, in the controller's isolated world: the element
// the last snapshot kept at `index`, scrolled into view, then one step:
// "box" (its centre, for a click through the browser's input, only when a
// click there lands on it or a label of it), "focus",
// "choose" (the option of a list whose text is `option`), or, for what the
// broker fills, "username", "password" or "code": focus, only if it's that
// kind of field.
(index, step, option) => {
  const element = (globalThis.__hatchling || [])[index];
  if (!element || !element.isConnected) {
    return { outcome: "unknown" };
  }
  element.scrollIntoView({ block: "center", inline: "center" });
  if (step === "box") {
    const box = element.getBoundingClientRect();
    if (box.width === 0 || box.height === 0) {
      return { outcome: "not_possible" };
    }
    const x = box.left + box.width / 2;
    const y = box.top + box.height / 2;
    // A click through the browser's input goes to whatever is on top at
    // that point: it's made only when that's the element, inside it, or
    // one of its labels. Anything over it (a banner, a frame, a layer the
    // page put there) would take the click, so none is made.
    const hit = document.elementFromPoint(x, y);
    const labels = element.labels ? Array.from(element.labels) : [];
    const lands =
      hit !== null &&
      (element === hit ||
        element.contains(hit) ||
        labels.some((label) => label === hit || label.contains(hit)));
    if (!lands) {
      return { outcome: "not_possible" };
    }
    return { outcome: "ok", x, y };
  }
  const kind = element.tagName === "INPUT" ? (element.getAttribute("type") || "text").toLowerCase() : "";
  const fits =
    (step === "username" && (kind === "text" || kind === "email")) ||
    (step === "password" && kind === "password") ||
    (step === "code" && (kind === "text" || kind === "tel" || kind === "number"));
  if (step === "focus" || fits) {
    element.focus();
    return { outcome: document.activeElement === element ? "ok" : "not_possible" };
  }
  if (step === "choose" && element.tagName === "SELECT") {
    const chosen = Array.from(element.options).find((item) => item.text.trim() === option);
    if (!chosen) {
      return { outcome: "not_possible" };
    }
    element.value = chosen.value;
    element.dispatchEvent(new Event("input", { bubbles: true }));
    element.dispatchEvent(new Event("change", { bubbles: true }));
    return { outcome: "ok" };
  }
  return { outcome: "not_possible" };
}
