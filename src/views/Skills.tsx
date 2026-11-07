import { Download, Loader2, Package, RefreshCw, Search, Sparkles, Trash2, Wand2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errorText, type HubSkill, type Skill } from "../lib/api";
import { Markdown } from "../components/Markdown";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, Empty, inputClass } from "../components/ui";

/**
 * Hermes skills: what the agent can load, read straight from the skills tree, plus hub search
 * and install through the Hermes CLI. Selecting a skill attaches it to the next chat turn.
 */
export function Skills({ onUseSkill }: { onUseSkill: (name: string) => void }) {
  const toast = useToast();
  const [skills, setSkills] = useState<Skill[] | null>(null);
  const [filter, setFilter] = useState("");
  const [category, setCategory] = useState<string>("");
  const [selected, setSelected] = useState<string | null>(null);
  const [body, setBody] = useState<string>("");
  const [loadingBody, setLoadingBody] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [hubQuery, setHubQuery] = useState("");
  const [hub, setHub] = useState<HubSkill[] | null>(null);
  const [searching, setSearching] = useState(false);

  const load = useCallback(async () => {
    try {
      setSkills(await api.skills());
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  useEffect(() => { void load(); }, [load]);

  useEffect(() => {
    if (!selected) {
      setBody("");
      return;
    }
    setLoadingBody(true);
    api
      .readSkill(selected)
      .then(setBody)
      .catch((e) => setBody(errorText(e)))
      .finally(() => setLoadingBody(false));
  }, [selected]);

  const categories = useMemo(() => Array.from(new Set((skills ?? []).map((s) => s.category))).sort(), [skills]);
  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase();
    return (skills ?? []).filter(
      (s) =>
        (!category || s.category === category) &&
        (!f || `${s.name} ${s.description} ${s.tags.join(" ")}`.toLowerCase().includes(f)),
    );
  }, [skills, filter, category]);

  async function searchHub() {
    if (!hubQuery.trim()) return;
    setSearching(true);
    try {
      setHub(await api.searchSkills(hubQuery.trim()));
    } catch (e) {
      toast("error", errorText(e));
      setHub([]);
    } finally {
      setSearching(false);
    }
  }

  async function install(id: string) {
    setBusy(id);
    try {
      toast("ok", await api.installSkill(id));
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function remove(name: string) {
    setBusy(name);
    try {
      toast("ok", await api.removeSkill(name));
      if (selected === name) setSelected(null);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const current = skills?.find((s) => s.name === selected) ?? null;

  return (
    <div className="flex h-full min-h-0">
      <aside className="flex w-80 shrink-0 flex-col border-r border-line">
        <div className="space-y-2 p-3">
          <div className="flex items-center gap-2">
            <div className="flex min-w-0 flex-1 items-center gap-2 rounded-lg bg-card px-2 ring-1 ring-line">
              <Search className="size-3.5 text-slate-500" />
              <input
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="Filter installed skills"
                className="h-8 min-w-0 flex-1 bg-transparent text-xs text-slate-100 outline-none placeholder:text-slate-600"
              />
            </div>
            <Button size="sm" variant="ghost" busy={busy === "update"} onClick={() => void (async () => {
              setBusy("update");
              try {
                toast("ok", await api.updateSkills());
                await load();
              } catch (e) {
                toast("error", errorText(e));
              } finally {
                setBusy(null);
              }
            })()} title="Update installed hub skills" icon={<RefreshCw className="size-3.5" />} />
          </div>
          <select value={category} onChange={(e) => setCategory(e.target.value)} className="h-8 w-full rounded-lg border border-line bg-card px-2 text-xs text-slate-300 outline-none" aria-label="Category">
            <option value="">All categories ({skills?.length ?? 0})</option>
            {categories.map((c) => (
              <option key={c} value={c}>
                {c}
              </option>
            ))}
          </select>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
          {!skills && <Loader2 className="mx-auto mt-6 size-4 animate-spin text-slate-500" />}
          {shown.map((s) => (
            <button
              key={s.name}
              onClick={() => setSelected(s.name)}
              className={`mb-0.5 block w-full rounded-lg px-2.5 py-2 text-left ${selected === s.name ? "bg-white/[0.07]" : "hover:bg-white/[0.03]"}`}
            >
              <div className="flex items-center gap-1.5">
                <span className="truncate text-[12.5px] text-slate-200">{s.name}</span>
                {s.version && <span className="shrink-0 text-[10px] text-slate-600">v{s.version}</span>}
              </div>
              <div className="line-clamp-2 text-[10.5px] leading-relaxed text-slate-500">{s.description || s.category}</div>
            </button>
          ))}
          {skills && shown.length === 0 && <div className="px-3 py-6 text-center text-[11.5px] text-slate-600">No skills match.</div>}
        </div>
      </aside>

      <section className="flex min-w-0 flex-1 flex-col">
        <div className="flex items-center gap-3 border-b border-line px-5 py-3">
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm font-semibold text-slate-100">{current?.name ?? "Skills"}</div>
            <div className="truncate text-[11px] text-slate-500">
              {current ? `${current.category} · ${current.path}` : "What Hermes can load. Installed from the skills tree; hub installs go through the Hermes CLI."}
            </div>
          </div>
          {current && (
            <>
              <Button size="sm" onClick={() => onUseSkill(current.name)} icon={<Sparkles className="size-3.5" />}>
                Use in a chat
              </Button>
              <Button size="sm" variant="danger" busy={busy === current.name} onClick={() => void remove(current.name)} icon={<Trash2 className="size-3.5" />}>
                Remove
              </Button>
            </>
          )}
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
          {current ? (
            loadingBody ? (
              <Loader2 className="mt-6 size-5 animate-spin text-slate-500" />
            ) : (
              <div className="mx-auto max-w-3xl">
                <div className="mb-3 flex flex-wrap gap-1.5">
                  {current.tags.map((t) => (
                    <Badge key={t} className="bg-white/5 text-slate-400 ring-white/10">
                      #{t}
                    </Badge>
                  ))}
                </div>
                <Markdown text={body} workingDir={null} />
              </div>
            )
          ) : (
            <div className="mx-auto max-w-3xl">
              <Card className="p-4">
                <div className="mb-2 flex items-center gap-2">
                  <Wand2 className="size-4 text-emerald-300" />
                  <span className="text-sm font-semibold text-slate-100">Find more skills</span>
                </div>
                <div className="flex gap-2">
                  <input
                    value={hubQuery}
                    onChange={(e) => setHubQuery(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && void searchHub()}
                    placeholder="Search the skill registries, for example: excel, kubernetes, figma"
                    className={inputClass}
                  />
                  <Button busy={searching} onClick={() => void searchHub()} icon={<Search className="size-4" />}>
                    Search
                  </Button>
                </div>
                {hub && (
                  <div className="mt-3 space-y-1.5">
                    {hub.length === 0 && <div className="text-[11.5px] text-slate-500">Nothing found.</div>}
                    {hub.map((h) => (
                      <div key={h.identifier} className="flex items-start gap-2 rounded-lg bg-canvas/60 px-3 py-2">
                        <Package className="mt-0.5 size-3.5 shrink-0 text-slate-500" />
                        <div className="min-w-0 flex-1">
                          <div className="flex items-center gap-1.5">
                            <span className="text-[12.5px] text-slate-200">{h.name}</span>
                            <Badge className="bg-white/5 text-slate-400 ring-white/10">{h.source}</Badge>
                            {h.trust_level && <Badge className="bg-sky-500/10 text-sky-300 ring-sky-500/25">{h.trust_level}</Badge>}
                          </div>
                          <div className="text-[10.5px] leading-relaxed text-slate-500">{h.description}</div>
                        </div>
                        <Button size="sm" busy={busy === h.identifier} onClick={() => void install(h.identifier)} icon={<Download className="size-3.5" />}>
                          Install
                        </Button>
                      </div>
                    ))}
                  </div>
                )}
              </Card>
              {!skills?.length && <Empty icon={<Package className="size-5" />} title="No skills installed" hint="Search above, or run hermes skills install in the terminal." />}
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
