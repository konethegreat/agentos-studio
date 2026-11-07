/**
 * Contextual guides: short hints that appear when the user's own state makes them relevant, and
 * stay gone once dismissed or acted on. Onboarding covers the first run; these cover the moments
 * after it (a chat filling up, missions queued with nobody to start them, a reply that runs long).
 *
 * This file is pure so the selection rules are tested without React (scripts/guides.test.ts).
 */

export type GuideId =
  | "chat.pick-folder"
  | "chat.fork"
  | "chat.long-turn"
  | "chat.context-full"
  | "missions.first"
  | "missions.queued-off"
  | "missions.review"
  | "memory.empty";

export interface GuideDef {
  id: GuideId;
  title: string;
  body: string;
  /** Help document that explains the feature in full, opened from the hint. */
  doc?: string;
}

/** Every guide, in priority order: when two are eligible at once, the earlier one is shown. */
export const GUIDES: GuideDef[] = [
  {
    id: "chat.context-full",
    title: "This chat is nearly full",
    body: "Hermes compacts older turns when the context window fills, and compacted turns lose detail. Fork at a recent reply to carry on with less history, or start a new chat for the next topic.",
    doc: "CHAT.md",
  },
  {
    id: "missions.queued-off",
    title: "Queued missions are waiting for a start",
    body: "Automations is off, so nothing will dispatch them. Turn Automations on and the scheduler starts them within a minute, one per repository, or press Start on a card.",
    doc: "AUTOMATIONS.md",
  },
  {
    id: "missions.review",
    title: "A mission is ready for review",
    body: "Agent review runs a second agent against the pull request diff and the acceptance criteria. Merging needs its approval, green CI, budget left and Allow merge.",
    doc: "AUTOMATIONS.md",
  },
  {
    id: "chat.long-turn",
    title: "Long reply in progress",
    body: "Continue in terminal opens this conversation with hermes --resume, where reasoning streams live and you can answer prompts directly. The chat here keeps going too.",
  },
  {
    id: "chat.pick-folder",
    title: "Pick a folder for this chat",
    body: "Chats start in your home folder. Choose a repository in the folder picker so the agent works inside it, and the chat files under that project.",
  },
  {
    id: "chat.fork",
    title: "Branch from any message",
    body: "Hover a message and press the fork icon. The new session keeps the conversation up to that point; forking at your own message puts it back in the composer to edit. The original chat is not changed.",
    doc: "CHAT.md",
  },
  {
    id: "missions.first",
    title: "Start with one mission",
    body: "A mission is a repository, a goal and acceptance criteria. A worker does the work and opens a pull request, a second agent reviews it against the criteria, and nothing merges without that approval.",
    doc: "AUTOMATIONS.md",
  },
  {
    id: "memory.empty",
    title: "The memory index is empty",
    body: "The vault is indexed when this tab loads, and agents add to it with remember. An empty index usually means the vault path in Setup points at the wrong folder. Once notes are indexed, agents recall decisions, conventions and gotchas by path and line.",
  },
];

export const guideById = (id: GuideId) => GUIDES.find((g) => g.id === id);

/** Preference keys, stored in studio.db kv through get_pref and set_pref. */
export const PREF_DISMISSED = "guides.dismissed";
export const PREF_OFF = "guides.off";

export function parseDismissed(raw: string | null | undefined): Set<GuideId> {
  if (!raw) return new Set();
  try {
    const v: unknown = JSON.parse(raw);
    if (!Array.isArray(v)) return new Set();
    const known = new Set<string>(GUIDES.map((g) => g.id));
    return new Set(v.filter((x): x is GuideId => typeof x === "string" && known.has(x)));
  } catch {
    return new Set();
  }
}

export const serializeDismissed = (s: Set<GuideId>) => JSON.stringify([...s].sort());

/**
 * The one guide to show, or null. Only one is visible at a time, so hints never stack up into a
 * wall of advice: the highest-priority eligible guide that is not dismissed wins.
 */
export function pickGuide(eligible: Iterable<GuideId>, dismissed: Set<GuideId>, off: boolean): GuideId | null {
  if (off) return null;
  const want = new Set(eligible);
  for (const g of GUIDES) if (want.has(g.id) && !dismissed.has(g.id)) return g.id;
  return null;
}

/** Inputs for the chat guides, so the thresholds live in one tested place. */
export interface ChatSignals {
  backend: "hermes" | "claude-code";
  hasSession: boolean;
  inHomeFolder: boolean;
  userTurns: number;
  streamingForMs: number;
  contextPct: number | null;
  compacted: number;
}

export const LONG_TURN_MS = 60_000;
export const CONTEXT_FULL_PCT = 75;
export const FORK_AFTER_TURNS = 3;

export function chatGuides(s: ChatSignals): GuideId[] {
  const out: GuideId[] = [];
  if (s.contextPct !== null && (s.contextPct >= CONTEXT_FULL_PCT || s.compacted > 0)) out.push("chat.context-full");
  if (s.streamingForMs >= LONG_TURN_MS) out.push("chat.long-turn");
  if (s.backend === "hermes" && !s.hasSession && s.inHomeFolder && s.userTurns === 0) out.push("chat.pick-folder");
  if (s.backend === "hermes" && s.hasSession && s.userTurns >= FORK_AFTER_TURNS && s.streamingForMs === 0) out.push("chat.fork");
  return out;
}

export interface MissionSignals {
  total: number;
  queued: number;
  awaitingReview: number;
  automationsOn: boolean | null;
}

export function missionGuides(s: MissionSignals): GuideId[] {
  const out: GuideId[] = [];
  if (s.total === 0) out.push("missions.first");
  if (s.queued > 0 && s.automationsOn === false) out.push("missions.queued-off");
  if (s.awaitingReview > 0) out.push("missions.review");
  return out;
}
