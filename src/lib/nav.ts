/** Switch to another screen from anywhere (empty states, tips). App listens for this event. */
export type NavTarget = "setup" | "chat" | "mission" | "control" | "help" | "fleet";

export function goTo(view: NavTarget) {
  window.dispatchEvent(new CustomEvent<NavTarget>("agentos:go", { detail: view }));
}
