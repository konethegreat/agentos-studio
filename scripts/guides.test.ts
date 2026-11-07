// Run: npm run test:guides  (node --experimental-strip-types --test scripts/guides.test.ts)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  GUIDES,
  chatGuides,
  missionGuides,
  parseDismissed,
  pickGuide,
  serializeDismissed,
  type ChatSignals,
} from "../src/lib/guides.ts";

const chat = (over: Partial<ChatSignals> = {}): ChatSignals => ({
  backend: "hermes",
  hasSession: true,
  inHomeFolder: false,
  userTurns: 1,
  streamingForMs: 0,
  contextPct: 10,
  compacted: 0,
  ...over,
});

test("guide ids are unique and every guide has text", () => {
  const ids = GUIDES.map((g) => g.id);
  assert.equal(new Set(ids).size, ids.length);
  for (const g of GUIDES) assert.ok(g.title.trim() && g.body.trim(), g.id);
});

test("guide text obeys the dash ban", () => {
  for (const g of GUIDES) assert.ok(!/[\u2013\u2014]/.test(g.title + g.body), g.id);
});

test("only one guide is picked, by priority, skipping dismissed ones", () => {
  const both = ["chat.fork", "chat.context-full"] as const;
  assert.equal(pickGuide(both, new Set(), false), "chat.context-full");
  assert.equal(pickGuide(both, new Set(["chat.context-full"]), false), "chat.fork");
  assert.equal(pickGuide(both, new Set(both), false), null);
  assert.equal(pickGuide(both, new Set(), true), null, "tips off hides everything");
  assert.equal(pickGuide([], new Set(), false), null);
});

test("dismissed set round-trips and ignores junk", () => {
  const s = parseDismissed(serializeDismissed(new Set(["chat.fork", "memory.empty"])));
  assert.deepEqual([...s].sort(), ["chat.fork", "memory.empty"]);
  assert.equal(parseDismissed("not json").size, 0);
  assert.equal(parseDismissed('{"a":1}').size, 0);
  assert.deepEqual([...parseDismissed('["chat.fork","no.such",3]')], ["chat.fork"]);
  assert.equal(parseDismissed(null).size, 0);
});

test("chat guides react to the chat's state", () => {
  assert.deepEqual(chatGuides(chat()), []);
  assert.ok(chatGuides(chat({ contextPct: 80 })).includes("chat.context-full"));
  assert.ok(chatGuides(chat({ compacted: 3 })).includes("chat.context-full"));
  assert.ok(!chatGuides(chat({ contextPct: null, compacted: 3 })).includes("chat.context-full"));
  assert.ok(chatGuides(chat({ streamingForMs: 61_000 })).includes("chat.long-turn"));
  assert.ok(!chatGuides(chat({ streamingForMs: 30_000 })).includes("chat.long-turn"));
  assert.ok(chatGuides(chat({ hasSession: false, inHomeFolder: true, userTurns: 0 })).includes("chat.pick-folder"));
  assert.ok(!chatGuides(chat({ hasSession: false, inHomeFolder: true, userTurns: 1 })).includes("chat.pick-folder"), "not once the first message is sent");
  assert.ok(!chatGuides(chat({ hasSession: false, inHomeFolder: false })).includes("chat.pick-folder"));
  assert.ok(chatGuides(chat({ userTurns: 3 })).includes("chat.fork"));
  assert.ok(!chatGuides(chat({ userTurns: 3, streamingForMs: 1 })).includes("chat.fork"), "not while streaming");
  assert.deepEqual(chatGuides(chat({ backend: "claude-code", userTurns: 0, hasSession: false, inHomeFolder: true })), []);
});

test("mission guides react to the board", () => {
  assert.deepEqual(missionGuides({ total: 0, queued: 0, awaitingReview: 0, automationsOn: false }), ["missions.first"]);
  assert.deepEqual(missionGuides({ total: 2, queued: 2, awaitingReview: 0, automationsOn: false }), ["missions.queued-off"]);
  assert.deepEqual(missionGuides({ total: 2, queued: 2, awaitingReview: 0, automationsOn: true }), []);
  assert.deepEqual(missionGuides({ total: 2, queued: 1, awaitingReview: 0, automationsOn: null }), [], "unknown status says nothing");
  assert.deepEqual(missionGuides({ total: 1, queued: 0, awaitingReview: 1, automationsOn: true }), ["missions.review"]);
});
