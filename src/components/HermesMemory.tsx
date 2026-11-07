import { Loader2, Pencil, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { errorText } from "../lib/api";
import { studio, type HermesMemory, type JourneyNode } from "../lib/studio";
import { ApprovalDeclined, useGuarded } from "./ApprovalProvider";
import { useCli } from "./CliDrawer";
import { useToast } from "./Toasts";
import { Badge, Button } from "./ui";

/** Hermes' built-in memory (MEMORY.md, USER.md) and its journey graph, beside the vault.
 *  Read-only here: changes go through `hermes journey edit` and `hermes journey delete`. */
export function HermesMemoryPanel() {
  const toast = useToast();
  const guarded = useGuarded();
  const cli = useCli();
  const [m, setM] = useState<HermesMemory | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const load = useCallback(() => {
    setM(null);
    void studio.hermes.memory().then(setM).catch((e) => toast("error", errorText(e)));
  }, [toast]);
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

  async function edit(n: JourneyNode) {
    // `hermes journey edit` opens $EDITOR, so it runs in a real terminal, typed for the user to confirm.
    const t = await cli.openPty({ profile: "shell", type_when_ready: `hermes journey edit ${n.id}` });
    if (t) toast("info", "A terminal opened with hermes journey edit typed. Press Enter to edit it in your editor.");
  }

  if (!m) return <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />;
  return (
    <div className="grid min-h-0 flex-1 grid-cols-1 divide-y divide-line overflow-y-auto lg:grid-cols-[1fr_1fr] lg:divide-x lg:divide-y-0">
      <div className="min-h-0 space-y-4 overflow-y-auto p-4">
        <div className="flex items-center gap-2">
          <p className="flex-1 text-small text-slate-500">Hermes injects these two files into every session. Hermes writes them through its memory tool; Studio only reads them.</p>
          <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={load}>
            Refresh
          </Button>
        </div>
        {[
          ["MEMORY.md", m.memory, m.memory_path],
          ["USER.md", m.user, m.user_path],
        ].map(([title, body, path]) => (
          <section key={title}>
            <div className="mb-1 flex items-baseline gap-2">
              <h3 className="text-xs font-semibold text-slate-200">{title}</h3>
              <span className="truncate font-mono text-caption text-slate-600" title={path}>
                {path}
              </span>
            </div>
            <pre className="max-h-72 overflow-auto whitespace-pre-wrap rounded-lg bg-inset p-3 font-mono text-label leading-relaxed text-slate-300">{body || "(empty)"}</pre>
          </section>
        ))}
        <section>
          <h3 className="mb-1 text-xs font-semibold text-slate-200">hermes memory status</h3>
          <pre className="overflow-auto rounded-lg bg-inset p-3 font-mono text-label leading-relaxed text-slate-400">{m.status.trim()}</pre>
        </section>
      </div>
      <div className="min-h-0 overflow-y-auto p-4">
        <h3 className="mb-1 text-xs font-semibold text-slate-200">Journey: learned skills and memories</h3>
        <p className="mb-3 text-small text-slate-500">From hermes journey --json, newest first. Edit and delete run the Hermes command; deleting a skill archives it.</p>
        {nodes.length === 0 && <p className="text-small text-slate-500">Nothing learned yet.</p>}
        <ul className="divide-y divide-line rounded-lg ring-1 ring-line">
          {nodes.map((n) => (
            <li key={n.id} className="flex items-center gap-2 px-3 py-2">
              <Badge className={n.kind === "skill" ? "bg-sky-500/10 text-sky-300 ring-sky-500/25" : "bg-violet-500/10 text-violet-300 ring-violet-500/25"}>{n.kind === "skill" ? "skill" : (n.memorySource ?? "memory")}</Badge>
              <div className="min-w-0 flex-1">
                <div className="truncate text-small text-slate-200" title={n.label}>
                  {n.label}
                </div>
                <div className="text-caption text-slate-600">
                  {n.timestamp ? new Date(n.timestamp * 1000).toLocaleDateString() : ""}
                  {n.kind === "skill" && n.useCount ? ` · used ${n.useCount}x` : ""}
                  {n.createdBy ? ` · ${n.createdBy}` : ""}
                </div>
              </div>
              <Button size="sm" variant="ghost" icon={<Pencil className="size-3.5" />} onClick={() => void edit(n)} aria-label={`Edit ${n.label}`} />
              <Button size="sm" variant="ghost" busy={busy === n.id} icon={<Trash2 className="size-3.5" />} onClick={() => void remove(n)} aria-label={`Delete ${n.label}`} />
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
