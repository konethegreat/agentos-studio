import { ArchiveRestore, BellOff, CheckCircle2, Clock, ExternalLink as ExternalIcon, HelpCircle, Loader2, RefreshCw, Undo2 } from "lucide-react";
import { useState } from "react";
import { Badge, Button, Card, ExternalLink } from "../components/ui";
import { useUpdates } from "../components/Updates";
import { useCli } from "../components/CliDrawer";
import type { BackupManifest, ProductUpdate } from "../lib/studio";

function when(ts: number | null | undefined) {
  if (!ts) return "never";
  return new Date(ts * 1000).toLocaleString();
}

function StateBadge({ p }: { p: ProductUpdate }) {
  if (p.state === "available") return <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/30">Update available</Badge>;
  if (p.state === "up_to_date") return <Badge className="bg-white/5 text-slate-300 ring-white/10">Up to date</Badge>;
  if (p.state === "not_installed") return <Badge className="bg-white/5 text-slate-400 ring-white/10">Not installed</Badge>;
  return <Badge className="bg-amber-500/10 text-amber-300 ring-amber-500/30">Could not check</Badge>;
}

function ProductCard({ p }: { p: ProductUpdate }) {
  const u = useUpdates();
  const [open, setOpen] = useState(p.state === "available");
  const cl = p.changelog;
  return (
    <Card className="p-5">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="text-sm font-semibold text-slate-100">{p.label}</h3>
            <StateBadge p={p} />
            {p.skipped && <Badge className="bg-white/5 text-slate-400 ring-white/10">Skipped {p.latest}</Badge>}
            {p.snoozed_until && <Badge className="bg-white/5 text-slate-400 ring-white/10">Reminding {when(p.snoozed_until)}</Badge>}
          </div>
          <div className="mt-1 text-xs text-slate-400">
            Installed <span className="font-mono text-slate-200">{p.current ?? "none"}</span>
            {p.latest && (
              <>
                {" "}
                · latest <span className="font-mono text-slate-200">{p.latest}</span>
              </>
            )}
          </div>
          <div className="mt-1 text-label text-slate-500">{p.detail}</div>
        </div>
        <div className="flex shrink-0 flex-wrap justify-end gap-1.5">
          {p.state === "available" && (
            <>
              <Button size="sm" variant="primary" busy={u.busy === p.product} disabled={!p.can_update || u.busy !== null} onClick={() => void u.apply(p.product)} title={p.update_note}>
                Update
              </Button>
              <Button size="sm" icon={<BellOff className="size-3.5" />} onClick={() => void u.skip(p)} title="Do not alert again for this version">
                Skip this version
              </Button>
              <Button size="sm" icon={<Clock className="size-3.5" />} onClick={() => void u.snooze(p.product, 24)} title="Hide the alert for 24 hours">
                Remind me later
              </Button>
            </>
          )}
          {(p.skipped || p.snoozed_until) && (
            <Button size="sm" variant="ghost" onClick={() => void u.clear(p.product)} title="Forget the skip or reminder for this product">
              Reset
            </Button>
          )}
        </div>
      </div>
      <p className="mt-3 text-label leading-relaxed text-slate-500">{p.update_note}</p>
      {cl && (
        <div className="mt-3 rounded-lg border border-line bg-canvas/60">
          <button onClick={() => setOpen((o) => !o)} className="flex w-full items-center justify-between px-3 py-2 text-left text-xs font-medium text-slate-200" aria-expanded={open}>
            <span>What changed{cl.versions.length ? ` (${cl.versions.join(", ")})` : ""}</span>
            <span className="text-slate-500">{open ? "Hide" : "Show"}</span>
          </button>
          {open && (
            <div className="space-y-3 border-t border-line px-3 py-3 text-xs">
              {cl.highlights.length > 0 && (
                <ul className="list-disc space-y-1 pl-4 text-slate-200">
                  {cl.highlights.map((h, i) => (
                    <li key={i}>{h}</li>
                  ))}
                </ul>
              )}
              {cl.groups.map((g) => (
                <div key={g.title}>
                  <div className="mb-1 text-label font-semibold uppercase tracking-wide text-slate-500">{g.title}</div>
                  <ul className="list-disc space-y-0.5 pl-4 text-slate-400">
                    {g.items.map((it, i) => (
                      <li key={i}>{it}</li>
                    ))}
                  </ul>
                </div>
              ))}
              {cl.url && (
                <ExternalLink href={cl.url} className="inline-flex items-center gap-1 no-underline">
                  Full release notes <ExternalIcon className="size-3" />
                </ExternalLink>
              )}
            </div>
          )}
        </div>
      )}
    </Card>
  );
}

function BackupRow({ b }: { b: BackupManifest }) {
  const u = useUpdates();
  const cli = useCli();
  return (
    <div className="flex items-start gap-3 border-b border-line px-4 py-3 last:border-0">
      <ArchiveRestore className="mt-0.5 size-4 shrink-0 text-slate-500" />
      <div className="min-w-0 flex-1 text-xs">
        <div className="text-slate-200">
          {b.product} <span className="font-mono">{b.from_version}</span> before updating to <span className="font-mono">{b.to_version}</span>
        </div>
        <div className="text-label text-slate-500">
          {when(b.created_at)}
          {b.hermes_commit && <> · commit {b.hermes_commit.slice(0, 10)}</>}
          {b.hermes_snapshot && <> · state snapshot {b.hermes_snapshot}</>}
        </div>
        <div className="text-label text-slate-500">{b.note}</div>
      </div>
      <div className="flex shrink-0 gap-1.5">
        {b.hermes_snapshot && (
          <Button
            size="sm"
            variant="ghost"
            title="Opens a Hermes terminal with the restore command typed in. Press Enter there to run it."
            onClick={() => {
              void cli.openPty({ profile: "hermes" }).then((t) => {
                if (t) window.setTimeout(() => cli.typeInto(t.id, `/snapshot restore ${b.hermes_snapshot}`), 4000);
              });
            }}
          >
            Restore state
          </Button>
        )}
        <Button size="sm" icon={<Undo2 className="size-3.5" />} disabled={u.busy !== null} onClick={() => void u.rollback(b)} title={`Roll back to ${b.from_version}`}>
          Roll back
        </Button>
      </div>
    </div>
  );
}

export function UpdateCenter() {
  const u = useUpdates();
  const order = ["hermes", "claude-code", "studio"];
  const products = [...u.products].sort((a, b) => order.indexOf(a.product) - order.indexOf(b.product));
  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-4xl space-y-5 px-6 py-6">
        <div className="flex items-center gap-3">
          <div className="flex-1">
            <h1 className="text-lg font-semibold text-slate-100">Update Center</h1>
            <p className="text-xs text-slate-500">Hermes, Claude Code and AgentOS Studio are checked at start and every 6 hours. Every update is backed up first so it can be rolled back.</p>
          </div>
          <Button icon={u.checking ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />} disabled={u.checking} onClick={() => void u.check()} title="Check all three now">
            Check now
          </Button>
        </div>
        {products.length === 0 && (
          <Card className="flex items-center gap-3 p-5 text-xs text-slate-400">
            {u.checking ? <Loader2 className="size-4 animate-spin" /> : <HelpCircle className="size-4" />}
            {u.checking ? "Checking versions..." : "No check has run yet. Press Check now."}
          </Card>
        )}
        {products.map((p) => (
          <ProductCard key={p.product} p={p} />
        ))}

        {u.log.length > 0 && (
          <Card className="p-0">
            <div className="flex items-center gap-2 border-b border-line px-4 py-2 text-xs font-medium text-slate-200">
              {u.busy ? <Loader2 className="size-3.5 animate-spin" /> : <CheckCircle2 className="size-3.5 text-emerald-300" />}
              Progress
            </div>
            <div className="max-h-80 overflow-y-auto px-4 py-2 font-mono text-small leading-relaxed">
              {u.log.map((l, i) => (
                <div
                  key={i}
                  className={
                    l.level === "error" ? "text-red-300" : l.level === "ok" ? "text-emerald-300" : l.level === "cmd" || l.level === "step" ? "text-sky-300" : l.level === "err" ? "text-amber-300/90" : "text-slate-300"
                  }
                >
                  {l.line || "\u00a0"}
                </div>
              ))}
            </div>
          </Card>
        )}

        <Card className="p-0">
          <div className="border-b border-line px-4 py-3">
            <h2 className="text-sm font-semibold text-slate-100">Backups and rollback</h2>
            <p className="text-label text-slate-500">Made automatically before every update. Rolling back asks for approval first.</p>
          </div>
          {u.backups.length === 0 ? <div className="px-4 py-6 text-center text-xs text-slate-600">No backups yet. One is made the first time you update.</div> : u.backups.map((b) => <BackupRow key={`${b.product}/${b.id}`} b={b} />)}
        </Card>
      </div>
    </div>
  );
}
