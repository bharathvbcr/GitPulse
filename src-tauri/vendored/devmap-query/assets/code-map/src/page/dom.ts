/**
 * Element construction for the page shell.
 *
 * Every string reaches the DOM as a text node or an attribute value, never as
 * markup: a repository names its own files, so `<img onerror=...>` in a path
 * must be inert by construction rather than by remembering to escape it.
 * This is the only module in the page layer that creates elements.
 */

export type Child = Node | string | number | null | undefined | false;

export interface Props {
  class?: string;
  text?: string;
  title?: string;
  /** Attributes set verbatim as string values (`aria-*`, `role`, `data-*`, `type`, ...). */
  attrs?: Record<string, string | number | boolean | undefined>;
  on?: Partial<{ [K in keyof HTMLElementEventMap]: (event: HTMLElementEventMap[K]) => void }>;
}

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Props = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  if (props.class) el.className = props.class;
  if (props.title) el.title = props.title;
  if (props.text !== undefined) el.textContent = props.text;
  for (const [name, value] of Object.entries(props.attrs ?? {})) {
    if (value === undefined || value === false) continue;
    el.setAttribute(name, value === true ? "" : String(value));
  }
  if (props.on) {
    for (const [type, handler] of Object.entries(props.on)) {
      if (handler) el.addEventListener(type, handler as EventListener);
    }
  }
  append(el, children);
  return el;
}

export function append(parent: Node, children: readonly Child[]): void {
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    parent.appendChild(typeof child === "object" ? child : document.createTextNode(String(child)));
  }
}

/** Replace everything inside `parent` with `children`. */
export function replaceChildren(parent: Element, ...children: Child[]): void {
  while (parent.firstChild) parent.removeChild(parent.firstChild);
  append(parent, children);
}

/** A button with an accessible name and a click handler. */
export function button(label: string, onClick: () => void, props: Props = {}): HTMLButtonElement {
  const attrs = { type: "button", ...props.attrs };
  return h("button", { ...props, attrs, text: props.text ?? label, on: { ...props.on, click: onClick } });
}

/** Number with thousands separators, stable across locales. */
export function fmt(n: number): string {
  return Math.round(n).toLocaleString("en-US");
}
