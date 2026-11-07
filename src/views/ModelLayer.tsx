import { ArrowDown, ArrowUp, FolderOpen, KeyRound, Layers, Loader2, Plus, RotateCcw, Route as RouteIcon, Trash2 } from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { ApprovalDeclined, useGuarded } from "../components/ApprovalProvider";
import { useCli } from "../components/CliDrawer";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, inputClass } from "../components/ui";
import { api, errorText } from "../lib/api";
import { studio, type Credential, type FolderModel, type Moa, type Route } from "../lib/studio";

type Catalog = { id: string; label: string; models: string[] }[];

/** Hermes' model layer: fallback chain, credential pools, mixture-of-agents presets, and Studio's
 *  per-folder default model. Every change runs a Hermes command or writes Studio's own store. */
export function ModelLayer() {
  const [catalog, setCatalog] = useState<Catalog>([]);
  useEffect(() => {
    void api
      .modelCatalog()
      .then((c) => setCatalog(c.providers))
      .catch(() => undefined);
  }, []);
  return (
    <div className="mt-6 space-y-4">
      <h2 className="text-sm font-semibold text-slate-100">Model layer</h2>
      <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
        <FallbackCard catalog={catalog} />
        <FolderDefaultsCard catalog={catalog} />
        <CredentialsCard />
        <MoaCard />
      </div>
    </div>
  );
}

function Section({ icon, title, hint, children, actions }: { icon: ReactNode; title: string; hint: string; children: ReactNode; actions?: ReactNode }) {
  return (
    <Card className="p-5">
      <div className="mb-3 flex items-start gap-3">
        <div className="grid size-8 shrink-0 place-items-center rounded-lg bg-emerald-500/10 ring-1 ring-emerald-500/25">{icon}</div>
        <div className="min-w-0 flex-1">
          <h3 className="text-sm font-semibold text-slate-100">{title}</h3>
          <p className="text-small text-slate-500">{hint}</p>
        </div>
        {actions}
      </div>
      {children}
    </Card>
  );
}

function ModelPicker({ catalog, value, onChange }: { catalog: Catalog; value: { provider: string; model: string }; onChange: (v: { provider: string; model: string }) => void }) {
  const models = catalog.find((p) => p.id === value.provider)?.models ?? [];
  return (
    <div className="flex gap-2">
      <select className={`${inputClass} w-44`} value={value.provider} onChange={(e) => onChange({ provider: e.target.value, model: "" })} aria-label="Provider">
        <option value="">Provider</option>
        {catalog.map((p) => (
          <option key={p.id} value={p.id}>
            {p.label}
          </option>
        ))}
      </select>
      <input className={`${inputClass} min-w-0 flex-1 font-mono text-xs`} list={`models-${value.provider}`} value={value.model} onChange={(e) => onChange({ ...value, model: e.target.value })} placeholder="model" aria-label="Model" />
      <datalist id={`models-${value.provider}`}>
        {models.map((m) => (
          <option key={m} value={m} />
        ))}
      </datalist>
    </div>
  );
}

function FallbackCard({ catalog }: { catalog: Catalog }) {
  const toast = useToast();
  const guarded = useGuarded();
  const [chain, setChain] = useState<Route[] | null>(null);
  const [draft, setDraft] = useState({ provider: "", model: "" });
  const [busy, setBusy] = useState(false);
  const load = useCallback(() => {
    void studio.models.fallbackChain().then(setChain).catch((e) => toast("error", errorText(e)));
  }, [toast]);
  useEffect(() => {
    load();
  }, [load]);
  async function save(next: Route[]) {
    setBusy(true);
    try {
      await guarded((a) => studio.models.setFallbackChain(next, a));
      toast("ok", next.length ? `Fallback chain saved (${next.length})` : "Fallback chain cleared");
      load();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }
  const move = (i: number, d: number) => {
    if (!chain) return;
    const n = [...chain];
    const [x] = n.splice(i, 1);
    n.splice(i + d, 0, x);
    void save(n);
  };
  return (
    <Section icon={<RouteIcon className="size-4 text-emerald-300" />} title="Fallback chain" hint="Tried in order when the main model is rate-limited, overloaded or unreachable. Saved to fallback_providers with hermes config set. A metered fallback costs money when it is used.">
      {!chain && <Loader2 className="size-4 animate-spin text-slate-500" />}
      {chain?.length === 0 && <p className="mb-2 text-small text-slate-500">No fallback: if the main model fails, the turn fails.</p>}
      {chain && chain.length > 0 && (
        <ol className="mb-3 divide-y divide-line rounded-lg ring-1 ring-line">
          {chain.map((r, i) => (
            <li key={`${r.provider}:${r.model}`} className="flex items-center gap-2 px-3 py-2 text-small">
              <span className="w-4 text-slate-500">{i + 1}</span>
              <span className="font-mono text-slate-200">{r.model}</span>
              <span className="text-slate-500">via {r.provider}</span>
              <div className="ml-auto flex">
                <Button size="sm" variant="ghost" disabled={busy || i === 0} icon={<ArrowUp className="size-3.5" />} onClick={() => move(i, -1)} aria-label="Move up" />
                <Button size="sm" variant="ghost" disabled={busy || i === chain.length - 1} icon={<ArrowDown className="size-3.5" />} onClick={() => move(i, 1)} aria-label="Move down" />
                <Button size="sm" variant="ghost" disabled={busy} icon={<Trash2 className="size-3.5" />} onClick={() => void save(chain.filter((_, j) => j !== i))} aria-label="Remove" />
              </div>
            </li>
          ))}
        </ol>
      )}
      <ModelPicker catalog={catalog} value={draft} onChange={setDraft} />
      <div className="mt-2">
        <Button
          size="sm"
          variant="primary"
          busy={busy}
          disabled={!chain || !draft.provider || !draft.model.trim()}
          icon={<Plus className="size-3.5" />}
          onClick={() => {
            void save([...(chain ?? []), { provider: draft.provider, model: draft.model.trim() }]);
            setDraft({ provider: "", model: "" });
          }}
        >
          Add to chain
        </Button>
      </div>
    </Section>
  );
}

function FolderDefaultsCard({ catalog }: { catalog: Catalog }) {
  const toast = useToast();
  const [list, setList] = useState<FolderModel[] | null>(null);
  const [draft, setDraft] = useState({ folder: "", provider: "", model: "" });
  const load = useCallback(() => {
    void studio.models.folderModels().then(setList).catch((e) => toast("error", errorText(e)));
  }, [toast]);
  useEffect(() => {
    load();
  }, [load]);
  async function save(next: FolderModel[]) {
    try {
      await studio.models.setFolderModels(next);
      load();
    } catch (e) {
      toast("error", errorText(e));
    }
  }
  return (
    <Section icon={<FolderOpen className="size-4 text-emerald-300" />} title="Folder defaults" hint="A new Hermes chat in this folder, or any folder inside it, starts on this model. The deepest matching folder wins; existing chats keep their model.">
      {list && list.length > 0 && (
        <ul className="mb-3 divide-y divide-line rounded-lg ring-1 ring-line">
          {list.map((f, i) => (
            <li key={f.folder} className="flex items-center gap-2 px-3 py-2 text-small">
              <span className="min-w-0 flex-1 truncate font-mono text-slate-300" title={f.folder}>
                {f.folder}
              </span>
              <span className="font-mono text-slate-200">{f.model}</span>
              <Button size="sm" variant="ghost" icon={<Trash2 className="size-3.5" />} onClick={() => void save(list.filter((_, j) => j !== i))} aria-label="Remove" />
            </li>
          ))}
        </ul>
      )}
      <div className="mb-2 flex gap-2">
        <input className={`${inputClass} min-w-0 flex-1 font-mono text-xs`} value={draft.folder} onChange={(e) => setDraft({ ...draft, folder: e.target.value })} placeholder="C:/Users/you/repo" aria-label="Folder" />
        <Button
          size="sm"
          icon={<FolderOpen className="size-3.5" />}
          onClick={() =>
            void openDialog({ directory: true, title: "Folder for a default model" }).then((d) => {
              if (typeof d === "string") setDraft((x) => ({ ...x, folder: d }));
            })
          }
        >
          Browse
        </Button>
      </div>
      <ModelPicker catalog={catalog} value={draft} onChange={(v) => setDraft({ ...draft, ...v })} />
      <div className="mt-2">
        <Button
          size="sm"
          variant="primary"
          disabled={!list || !draft.folder.trim() || !draft.provider || !draft.model.trim()}
          icon={<Plus className="size-3.5" />}
          onClick={() => {
            const next = [...(list ?? []).filter((f) => f.folder !== draft.folder.trim()), { folder: draft.folder.trim(), provider: draft.provider, model: draft.model.trim() }];
            void save(next);
            setDraft({ folder: "", provider: "", model: "" });
          }}
        >
          Set default
        </Button>
      </div>
    </Section>
  );
}

function CredentialsCard() {
  const toast = useToast();
  const guarded = useGuarded();
  const cli = useCli();
  const [creds, setCreds] = useState<Credential[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const load = useCallback(() => {
    void studio.models.credentials().then(setCreds).catch((e) => toast("error", errorText(e)));
  }, [toast]);
  useEffect(() => {
    load();
  }, [load]);
  async function act(c: Credential, action: "priority" | "reset" | "remove", priority?: number) {
    setBusy(c.id + action);
    try {
      const msg = await guarded((a) => studio.models.credentialAction(c.provider, c.id, action, priority ?? null, a));
      toast("ok", msg || "Done");
      load();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }
  const providers = [...new Set((creds ?? []).map((c) => c.provider))];
  return (
    <Section
      icon={<KeyRound className="size-4 text-emerald-300" />}
      title="Credential pools"
      hint="Several keys or logins per provider; Hermes rotates to the next when one is exhausted. Keys never pass through Studio: Add opens hermes auth add in a terminal, where Hermes prompts for the secret."
      actions={
        <Button size="sm" icon={<Plus className="size-3.5" />} onClick={() => void cli.openPty({ profile: "shell", type_when_ready: "hermes auth add " }).then((t) => t && toast("info", "Type the provider id after hermes auth add, then press Enter. Hermes asks for the key itself."))}>
          Add
        </Button>
      }
    >
      {!creds && <Loader2 className="size-4 animate-spin text-slate-500" />}
      {creds?.length === 0 && <p className="text-small text-slate-500">No pooled credentials.</p>}
      {providers.map((p) => {
        const list = (creds ?? []).filter((c) => c.provider === p).sort((a, b) => a.priority - b.priority);
        return (
          <div key={p} className="mb-3">
            <div className="mb-1 text-label font-medium uppercase tracking-wide text-slate-500">{p}</div>
            <ul className="divide-y divide-line rounded-lg ring-1 ring-line">
              {list.map((c, i) => (
                <li key={c.id} className="flex items-center gap-2 px-3 py-2 text-small">
                  <span className="min-w-0 flex-1 truncate text-slate-200" title={`${c.label} (${c.source})`}>
                    {c.label || c.id}
                  </span>
                  <Badge className="bg-white/5 text-slate-400 ring-white/10">{c.kind}</Badge>
                  {c.current && <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/25">next</Badge>}
                  {c.status && <span className="truncate text-label text-amber-300">{c.status}</span>}
                  <Button size="sm" variant="ghost" disabled={i === 0 || busy !== null} icon={<ArrowUp className="size-3.5" />} onClick={() => void act(c, "priority", Math.max(0, c.priority - 1))} aria-label="Try earlier" />
                  <Button size="sm" variant="ghost" busy={busy === c.id + "reset"} icon={<RotateCcw className="size-3.5" />} onClick={() => void act(c, "reset")} aria-label="Clear exhaustion" />
                  <Button size="sm" variant="ghost" busy={busy === c.id + "remove"} icon={<Trash2 className="size-3.5" />} onClick={() => void act(c, "remove")} aria-label="Remove" />
                </li>
              ))}
            </ul>
          </div>
        );
      })}
    </Section>
  );
}

function MoaCard() {
  const toast = useToast();
  const cli = useCli();
  const [moa, setMoa] = useState<Moa | null>(null);
  const load = useCallback(() => {
    void studio.models.moa().then(setMoa).catch((e) => toast("error", errorText(e)));
  }, [toast]);
  useEffect(() => {
    load();
  }, [load]);
  async function activate(name: string) {
    try {
      await studio.models.setActiveMoa(name);
      toast("ok", name ? `Mixture of agents on: ${name}` : "Mixture of agents off");
      load();
    } catch (e) {
      toast("error", errorText(e));
    }
  }
  return (
    <Section
      icon={<Layers className="size-4 text-emerald-300" />}
      title="Mixture of agents"
      hint="Reference models advise and an aggregator answers, through Hermes' /moa. Activating a preset applies it to every turn and multiplies the cost of each; presets are edited with hermes moa configure."
      actions={
        <Button size="sm" icon={<Plus className="size-3.5" />} onClick={() => void cli.openPty({ profile: "shell", type_when_ready: "hermes moa configure " })}>
          Configure
        </Button>
      }
    >
      {!moa && <Loader2 className="size-4 animate-spin text-slate-500" />}
      {moa && (
        <>
          <p className="mb-2 text-small text-slate-400">Active: {moa.active_preset || "off"}</p>
          <ul className="space-y-2">
            {moa.presets.map((p) => (
              <li key={p.name} className="rounded-lg px-3 py-2 ring-1 ring-line">
                <div className="flex items-center gap-2">
                  <span className="text-small font-medium text-slate-200">{p.name}</span>
                  {moa.active_preset === p.name && <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/25">active</Badge>}
                  <div className="ml-auto">
                    {moa.active_preset === p.name ? (
                      <Button size="sm" onClick={() => void activate("")}>
                        Turn off
                      </Button>
                    ) : (
                      <Button size="sm" onClick={() => void activate(p.name)}>
                        Use for every turn
                      </Button>
                    )}
                  </div>
                </div>
                <div className="mt-1 text-label text-slate-500">
                  Advisors: {p.reference_models.map((r) => `${r.provider}:${r.model}`).join(", ") || "none"}
                  {p.aggregator ? ` · aggregator ${p.aggregator.provider}:${p.aggregator.model}` : ""}
                </div>
              </li>
            ))}
          </ul>
        </>
      )}
    </Section>
  );
}
