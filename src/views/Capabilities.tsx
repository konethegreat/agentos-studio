import { Copy, Layers, Loader2, RefreshCw, Search, ShieldAlert } from "lucide-react";
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { api, errorText, type CatalogIndex, type CatalogRecord } from "../lib/api";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, Empty, inputClass } from "../components/ui";

const KINDS = ["skill", "plugin", "agent", "command", "mcp"] as const;
const SOURCES = ["hermes", "bridge", "qoder", "claude", "local", "git"] as const;

const SOURCE_STYLE: Record<string, string> = {
  hermes: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25",
  bridge: "bg-teal-500/10 text-teal-300 ring-teal-500/25",
  qoder: "bg-sky-500/10 text-sky-300 ring-sky-500/25",
  claude: "bg-amber-500/10 text-amber-300 ring-amber-500/25",
  local: "bg-white/5 text-slate-400 ring-white/10",
  git: "bg-violet-500/10 text-violet-300 ring-violet-500/25",
};

function SourceBadge({ r }: { r: CatalogRecord }) {
  return (
    <Badge className={SOURCE_STYLE[r.source] ?? SOURCE_STYLE.local}>
      {r.source}
      {!r.installed && " (listed)"}
    </Badge>
  );
}

/**
 * The capability fabric: one read-only index of every skill, plugin, agent, command and MCP
 * server across Hermes, Qoder and Claude Code. Studio reads frontmatter and manifests only and
 * never writes to a source store. Same-named skills form a duplicate group with the precedence
 * a runtime should follow: Hermes native, Studio's bridge, Qoder, Claude Code, other roots.
 * K-ONE IT SOLUTIONS, Kone Tshivhinda.
 */
export function Capabilities() {
  const toast = useToast();
  const [idx, setIdx] = useState<CatalogIndex | null>(null);
  const [loading, setLoading] = useState(false);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState("");
  const [source, setSource] = useState("");
  const [dupesOnly, setDupesOnly] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setIdx(await api.catalogIndex());
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setLoading(false);
    }
  }, [toast]);

  useEffect(() => {
    void load();
  }, [load]);

  const byId = useMemo(() => new Map((idx?.records ?? []).map((r) => [r.id, r])), [idx]);
  const groupOf = useMemo(() => {
    const m = new Map<string, string[]>();
    for (const g of idx?.groups ?? []) for (const id of g.members) m.set(id, g.members);
    return m;
  }, [idx]);

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    return (idx?.records ?? [])
      .filter((r) => (!kind || r.kind === kind) && (!source || r.source === source))
      .filter((r) => !dupesOnly || groupOf.has(r.id))
      .filter((r) => !q || r.name.toLowerCase().includes(q) || r.description.toLowerCase().includes(q) || (r.plugin ?? "").toLowerCase().includes(q))
      .sort((a, b) => a.name.localeCompare(b.name) || a.source.localeCompare(b.source));
  }, [idx, query, kind, source, dupesOnly, groupOf]);

  const current = selected ? byId.get(selected) : undefined;
  const group = current ? groupOf.get(current.id) : undefined;

  function copy(text: string, what: string) {
    void navigator.clipboard.writeText(text).then(
      () => toast("ok", `${what} copied`),
      () => toast("error", "Could not copy"),
    );
  }

  return (
    <div className="flex h-full min-h-0">
      <aside className="flex w-96 shrink-0 flex-col border-r border-line">
        <div className="space-y-2 p-3">
          <div className="flex gap-2">
            <div className="relative flex-1">
              <Search className="pointer-events-none absolute left-2.5 top-2.5 size-3.5 text-slate-500" />
              <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search name, description or plugin" className={`${inputClass} pl-8`} aria-label="Search capabilities" />
            </div>
            <Button size="sm" busy={loading} onClick={() => void load()} title="Scan again" icon={<RefreshCw className="size-3.5" />} />
          </div>
          <div className="flex gap-2">
            <select value={kind} onChange={(e) => setKind(e.target.value)} className="h-8 flex-1 rounded-lg border border-line bg-card px-2 text-xs text-slate-300 outline-none" aria-label="Kind">
              <option value="">All kinds</option>
              {KINDS.map((k) => (
                <option key={k} value={k}>
                  {k}
                </option>
              ))}
            </select>
            <select value={source} onChange={(e) => setSource(e.target.value)} className="h-8 flex-1 rounded-lg border border-line bg-card px-2 text-xs text-slate-300 outline-none" aria-label="Source">
              <option value="">All sources</option>
              {SOURCES.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </div>
          <label className="flex items-center gap-2 text-label text-slate-400">
            <input type="checkbox" checked={dupesOnly} onChange={(e) => setDupesOnly(e.target.checked)} />
            Only skills that exist in more than one store ({idx?.groups.length ?? 0} groups)
          </label>
          <div className="text-label text-slate-500">
            {shown.length} of {idx?.records.length ?? 0} shown
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
          {!idx && (
            <div className="mt-6 flex flex-col items-center gap-2 px-4 text-center text-label text-slate-500">
              <Loader2 className="size-4 animate-spin" />
              The first scan reads every folder once to hash it, which can take a couple of minutes. Later scans only reread folders that changed.
            </div>
          )}
          {shown.slice(0, 600).map((r) => (
            <button key={r.id} onClick={() => setSelected(r.id)} className={`mb-0.5 block w-full rounded-lg px-2.5 py-2 text-left ${selected === r.id ? "bg-white/[0.07]" : "hover:bg-white/[0.03]"}`}>
              <div className="flex items-center gap-1.5">
                <span className="truncate text-body-sm text-slate-200">{r.name}</span>
                <span className="shrink-0 text-2xs text-slate-600">{r.kind}</span>
                {groupOf.has(r.id) && <Layers className="size-3 shrink-0 text-amber-400" aria-label="Duplicate group" />}
                {r.compat.length > 0 && <ShieldAlert className="size-3 shrink-0 text-slate-500" aria-label="Compatibility notes" />}
                <span className="ml-auto shrink-0">
                  <SourceBadge r={r} />
                </span>
              </div>
              <div className="line-clamp-2 text-caption leading-relaxed text-slate-500">{r.description || r.plugin || r.source_path}</div>
            </button>
          ))}
          {shown.length > 600 && <div className="px-3 py-2 text-center text-label text-slate-600">Showing the first 600. Narrow the search to see the rest.</div>}
          {idx && shown.length === 0 && <div className="px-3 py-6 text-center text-small text-slate-600">Nothing matches.</div>}
        </div>
      </aside>

      <section className="min-w-0 flex-1 overflow-y-auto px-6 py-5">
        <div className="mx-auto max-w-3xl space-y-4">
          {!current && (
            <>
              <div>
                <div className="text-sm font-semibold text-slate-100">Capabilities</div>
                <div className="text-label text-slate-500">
                  Read only. Studio reads frontmatter and manifests and never writes to Hermes, Qoder or Claude Code. Duplicates are shown with precedence, never merged.
                </div>
              </div>
              <Card className="p-4">
                <div className="mb-2 text-xs font-semibold text-slate-300">Per store</div>
                <table className="w-full text-label">
                  <thead>
                    <tr className="text-slate-500">
                      <th className="py-1 text-left font-medium">Source</th>
                      {KINDS.map((k) => (
                        <th key={k} className="py-1 text-right font-medium">
                          {k}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {Object.entries(idx?.counts ?? {}).map(([s, c]) => (
                      <tr key={s} className="border-t border-line text-slate-300">
                        <td className="py-1">{s}</td>
                        {KINDS.map((k) => (
                          <td key={k} className="py-1 text-right tabular-nums">
                            {c[k] ?? 0}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </Card>
              <Card className="p-4">
                <div className="mb-1 text-xs font-semibold text-slate-300">Precedence for duplicate skills</div>
                <div className="text-label text-slate-500">Hermes native, then Studio's bridge, then Qoder, then Claude Code, then other local roots. The first copy in a group is the one a runtime should prefer.</div>
              </Card>
              {(idx?.skipped.length ?? 0) > 0 && (
                <Card className="p-4">
                  <div className="mb-1 text-xs font-semibold text-slate-300">Not read</div>
                  <ul className="space-y-0.5 text-label text-slate-500">
                    {idx!.skipped.slice(0, 30).map((s) => (
                      <li key={s} className="break-all">
                        {s}
                      </li>
                    ))}
                  </ul>
                </Card>
              )}
              {idx && idx.records.length === 0 && <Empty icon={<Layers className="size-5" />} title="No capabilities found" hint="None of the store folders exist on this machine." />}
            </>
          )}
          {current && (
            <>
              <div className="flex items-start gap-3">
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="text-sm font-semibold text-slate-100">{current.name}</span>
                    <Badge className="bg-white/5 text-slate-400 ring-white/10">{current.kind}</Badge>
                    <SourceBadge r={current} />
                    {current.version && <span className="text-2xs text-slate-500">v{current.version}</span>}
                  </div>
                  <div className="mt-1 text-body-sm leading-relaxed text-slate-400">{current.description || "No description."}</div>
                </div>
                <Button size="sm" variant="ghost" onClick={() => setSelected(null)}>
                  Overview
                </Button>
              </div>
              <Card className="space-y-1.5 p-4 text-label">
                <Row label="File">
                  <span className="break-all text-slate-300">{current.source_path}</span>
                </Row>
                {current.plugin && <Row label="Plugin">{current.plugin}</Row>}
                <Row label="Publisher">{current.publisher ?? "not stated"}</Row>
                <Row label="License">{current.license ?? "not stated"}</Row>
                {current.homepage && <Row label="Homepage">{current.homepage}</Row>}
                <Row label="Hash">
                  <span className="font-mono text-slate-300">{current.hash.slice(0, 24)}</span>
                  <button onClick={() => copy(current.hash, "Hash")} className="ml-2 inline-flex items-center gap-1 text-slate-500 hover:text-slate-200" aria-label="Copy hash">
                    <Copy className="size-3" />
                  </button>
                </Row>
                <Row label="Asks for">{current.capabilities.length ? current.capabilities.join(", ") : "nothing declared"}</Row>
              </Card>
              <Card className="p-4">
                <div className="mb-1 text-xs font-semibold text-slate-300">Compatibility</div>
                {current.compat.length === 0 ? (
                  <div className="text-label text-slate-500">No notes: the frontmatter fields are ones the runtimes read, referenced files exist and no absolute paths were found.</div>
                ) : (
                  <ul className="space-y-0.5 text-label text-slate-400">
                    {current.compat.map((c) => (
                      <li key={c}>{c}</li>
                    ))}
                  </ul>
                )}
                {current.kind === "skill" && <div className="mt-2 text-label text-slate-600">Every runtime reads name and description; other fields are read only where the runtime supports them.</div>}
              </Card>
              {group && (
                <Card className="p-4">
                  <div className="mb-1 text-xs font-semibold text-slate-300">Duplicate group, in precedence order</div>
                  <ol className="space-y-1 text-label">
                    {group.map((id, i) => {
                      const m = byId.get(id);
                      if (!m) return null;
                      return (
                        <li key={id}>
                          <button onClick={() => setSelected(id)} className={`flex w-full items-center gap-2 rounded px-1.5 py-1 text-left ${id === current.id ? "bg-white/[0.06]" : "hover:bg-white/[0.03]"}`}>
                            <span className="w-4 text-slate-600">{i + 1}</span>
                            <SourceBadge r={m} />
                            <span className="truncate text-slate-400">{m.source_path}</span>
                            {i === 0 && <span className="ml-auto shrink-0 text-emerald-300">preferred</span>}
                            {m.hash !== byId.get(group[0])?.hash && i > 0 && <span className="ml-auto shrink-0 text-amber-300">differs</span>}
                          </button>
                        </li>
                      );
                    })}
                  </ol>
                </Card>
              )}
            </>
          )}
          <div className="text-2xs text-slate-600">Capability fabric by K-ONE IT SOLUTIONS, Kone Tshivhinda.</div>
        </div>
      </section>
    </div>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex gap-3">
      <span className="w-20 shrink-0 text-slate-500">{label}</span>
      <span className="min-w-0 flex-1 text-slate-300">{children}</span>
    </div>
  );
}
