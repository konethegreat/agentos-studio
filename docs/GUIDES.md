# Tips and guides

Studio shows one short tip at a time, only when your own state makes it relevant. A tip disappears
for good when you press Got it, or when you do what it suggests. The first-run guide covers setup;
these cover the moments after it.

| Tip | Shows when | Where |
|---|---|---|
| This chat is nearly full | the context window is at least 75% full, or Hermes has compacted messages | Chat |
| Queued missions are waiting for a start | missions are queued and Automations is off | Missions |
| A mission is ready for review | a mission is in review with no verdict yet | Missions |
| Long reply in progress | a reply has been running for a minute | Chat |
| Pick a folder for this chat | a new Hermes chat would start in your home folder | Chat |
| Branch from any message | a Hermes chat has three or more of your messages | Chat |
| Start with one mission | the Missions board is empty | Missions |
| The memory index is empty | the Memory tab finds no indexed notes | Memory |

When two tips apply at once, the one higher in this table wins, so tips never stack up.

## Turning tips off or back on

Help has a Tips switch and a Show tips again button. Show tips again also brings the first-run guide
back on the next start. Both are stored in `studio.db` (`guides.off`, `guides.dismissed`,
`onboarding.done`), so they survive restarts and updates.

## For contributors

The rules live in `src/lib/guides.ts` and are tested by `npm run test:guides`. A view reports which
tips apply with `useReportGuides(owner, ids, visible)` and places `<Guide id=... />` where the tip
should appear; the provider decides which single tip is visible. A view that stays mounted while
hidden must pass `visible = false`, or its tips would show on another screen.
