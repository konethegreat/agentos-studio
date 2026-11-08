import { AlertTriangle, Loader2, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { errorText } from "../lib/api";
import { studio, type HermesMemory, type JourneyNode } from "../lib/studio";
import { ApprovalDeclined, useGuarded } from "./ApprovalProvider";
import { useToast } from "./Toasts";
import { Badge, Button } from "./ui";

function ago(secs: number): string {
  const d = Math.max(0, Math.floor(Date.now() / 1000) - secs);
  if (d < 60) return `${d}s ago`;
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86_400) return `${Math.floor(d / 3600)}h ago`;
  return `${Math.floor(d / 86_400)}d ago`;
}

function stamp(secs: number | null | undefined): string {
  if (!secs) return "unknown time";
  return `${new Date(secs * 1000).toLocaleString()} (${ago(secs)})`;
}

function Unavailable({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="flex items-start gap-2 rounded-lg bg-amber-500/10 p-3 ring-1 ring-amber-500/25">
      <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-amber-300" />
      <div className="min-w-0 text-small">
        <div className="font-medium text-amber-200">{title}</div>
        <div className="mt-0.5 break-words text-amber-200/80">{detail}</div>
      </div>
    </div>
  );
}

/** Hermes' own store: MEMORY.md, USER.md, memory status and the journey graph.
 *  Studio only reads these files; delete runs `hermes journey delete` behind the
 *  `hermes.journey.delete` permission. */
export function HermesMemoryPanel() {
  const toast = useToast();
  const guarded = useGuarded();
  const [m, setM] = useState<HermesMemory | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const load = useCallback(() => {
    setM(null);
    setError(null);
    void studio.hermes
      .memory()
      .then(setM)
      .catch((e) => setError(errorText(e)));
  }, []);
  useEffect(() => {
    load();
  }, [load]);

  const nodes: JourneyNode[] = useMemo(() => {
    const all = m?.journey?.nodes ?? [];
    return [...all].sort((a, b) => (b.timestamp ?? 0) - (a.timestamp ?? 0));
  }, [m]);

  async function remove(n: JourneyNode) {
    setBusy(n.id);
    try {
      toast("ok", (await guarded((a) => studio.hermes.journeyDelete(n.id, a))) || "Deleted");
      load();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  if (error) {
    return (
      <div className="space-y-3 p-4">
        <Unavailable title="Hermes memory is unavailable" detail={error} />
        <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={load}>
          Try again
        </Button>
      </div>
    );
  }

  if (!m) {
    return (
      <div className="flex items-center justify-center gap-2 p-10 text-sm text-slate-500">
        <Loader2 className="size-4 animate-spin" /> Reading the Hermes store...
      </div>
    );
  }

  return (
    <div className="min-h-0 space-y-4 overflow-y-auto p-4">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <Badge className="bg-violet-500/10 text-violet-300 ring-violet-500/25">Hermes</Badge>
            <span className="text-small font-medium text-slate-300">Hermes store</span>
          </div>
          <p className="mt-1 truncate font-mono text-caption text-slate-600" title={m.home}>
            {m.home}
          </p>
        </div>
        <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={load}>
          Refresh
        </Button>
      </div>

      {[
        { title: "MEMORY.md", body: m.memory, path: m.memory_path, present: m.memory_present, modified: m.memory_modified },
        { title: "USER.md", body: m.user, path: m.user_path, present: m.user_present, modified: m.user_modified },
      ].map((f) => (
        <section key={f.title}>
          <div className="mb-1 flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
            <h3 className="text-xs font-semibold text-slate-200">{f.title}</h3>
            <span className="rounded-full bg-violet-500/10 px-1.5 py-0.5 text-2xs text-violet-300 ring-1 ring-violet-500/25">
              Hermes
            </span>
            <span className="text-caption text-slate-500">changed {stamp(f.modified)}</span>
          </div>
          <div className="mb-1 truncate font-mono text-caption text-slate-600" title={f.path}>
            {f.path}
          </div>
          {f.present ? (
            <pre className="max-h-72 overflow-auto whitespace-pre-wrap rounded-lg bg-inset p-3 font-mono text-label leading-relaxed text-slate-300">
              {f.body || "(empty)"}
            </pre>
          ) : (
            <Unavailable title={`${f.title} is missing`} detail={`Not found at ${f.path}. Hermes creates it on first use.`} />
          )}
        </section>
      ))}

      <section>
        <h3 className="mb-1 text-xs font-semibold text-slate-200">hermes memory status</h3>
        {m.status_error ? (
          <Unavailable title="hermes memory status failed" detail={m.status_error} />
        ) : (
          <pre className="overflow-auto rounded-lg bg-inset p-3 font-mono text-label leading-relaxed text-slate-400">
            {m.status.trim() || "(no output)"}
          </pre>
        )}
      </section>

      <section>
        <div className="mb-1 flex items-center gap-2">
          <h3 className="text-xs font-semibold text-slate-200">Journey: learned skills and memories</h3>
          <span className="text-caption text-slate-500">{nodes.length} entries</span>
        </div>
        <p className="mb-2 text-small text-slate-500">
          From hermes journey --json, newest first. Delete runs the Hermes command behind the
          hermes.journey.delete permission; deleting a skill archives it.
        </p>
        {m.journey_error ? (
          <Unavailable title="The journey graph is unavailable" detail={m.journey_error} />
        ) : nodes.length === 0 ? (
          <p className="text-small text-slate-500">Nothing learned yet. Hermes adds entries as it works.</p>
        ) : (
          <ul className="divide-y divide-line rounded-lg ring-1 ring-line">
            {nodes.map((n) => (
              <li key={n.id} className="flex items-center gap-2 px-3 py-2">
                <span className="shrink-0 rounded-full bg-violet-500/10 px-1.5 py-0.5 text-2xs text-violet-300 ring-1 ring-violet-500/25">
                  Hermes
                </span>
                <Badge
                  className={
                    n.kind === "skill"
                      ? "bg-sky-500/10 text-sky-300 ring-sky-500/25"
                      : "bg-violet-500/10 text-violet-300 ring-violet-500/25"
                  }
                >
                  {n.kind === "skill" ? "skill" : (n.memorySource ?? "memory")}
                </Badge>
                <div className="min-w-0 flex-1">
                  <div className="truncate text-small text-slate-200" title={n.label}>
                    {n.label}
                  </div>
                  <div className="truncate text-caption text-slate-600">
                    {n.timestamp ? stamp(n.timestamp) : "date unknown"}
                    {n.kind === "skill" && n.useCount ? ` · used ${n.useCount}x` : ""}
                    {n.createdBy ? ` · ${n.createdBy}` : ""}
                  </div>
                </div>
                <Button
                  size="sm"
                  variant="ghost"
                  busy={busy === n.id}
                  icon={<Trash2 className="size-3.5" />}
                  onClick={() => void remove(n)}
                  aria-label={`Delete ${n.label}`}
                />
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
