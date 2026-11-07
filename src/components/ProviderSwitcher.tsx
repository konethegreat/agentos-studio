import { Check, ChevronDown, Loader2, Plug, Search, Sparkles } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { api, errorText, type HermesStatus, type McpServer, type ProviderConfig } from "../lib/api";
import { useToast } from "./Toasts";
import { Toggle } from "./ui";

/** Header pill: shows the engine and lets you switch provider, model or MCP servers mid-conversation. */
export function ProviderSwitcher({
  backend,
  hermes,
  onSwitched,
  onOpenControl,
}: {
  backend: "hermes" | "claude-code";
  hermes: HermesStatus | null;
  onSwitched: () => void;
  onOpenControl: () => void;
}) {
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [cfg, setCfg] = useState<ProviderConfig | null>(null);
  const [servers, setServers] = useState<McpServer[]>([]);
  const [provider, setProvider] = useState<string>("");
  const [models, setModels] = useState<string[]>([]);
  const [filter, setFilter] = useState("");
  const [loadingModels, setLoadingModels] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (root.current && !root.current.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    void Promise.all([api.providerConfig(), api.mcpServers()])
      .then(([c, s]) => {
        setCfg(c);
        setServers(s.filter((x) => x.agents.some((a) => a.agent === "hermes")));
        setProvider((p) => p || c.active_id || c.providers[0]?.id || "");
      })
      .catch((e) => toast("error", errorText(e)));
    return () => window.removeEventListener("mousedown", close);
  }, [open, toast]);

  useEffect(() => {
    if (!open || !provider) return;
    setLoadingModels(true);
    setFilter("");
    api
      .models(provider)
      .then(setModels)
      .catch((e) => toast("error", errorText(e)))
      .finally(() => setLoadingModels(false));
  }, [open, provider, toast]);

  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase();
    const list = f ? models.filter((m) => m.toLowerCase().includes(f)) : models;
    return list.slice(0, 120);
  }, [models, filter]);

  async function choose(model: string) {
    setBusy(model);
    try {
      const r = await api.setProvider(provider, model, null, null);
      setCfg(r.config);
      toast("ok", r.log[r.log.length - 1] ?? "Switched");
      onSwitched();
      setOpen(false);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function toggleServer(name: string, enabled: boolean) {
    setBusy(name);
    try {
      toast("ok", await api.setMcpEnabled(name, enabled));
      setServers(await api.mcpServers().then((s) => s.filter((x) => x.agents.some((a) => a.agent === "hermes"))));
      onSwitched();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const label =
    backend === "claude-code" ? "Claude Code (your Claude login)" : `${hermes?.provider_label ?? "Hermes"}${hermes?.model ? ` (${hermes.model})` : ""}`;
  const current = cfg?.providers.find((p) => p.id === provider);

  return (
    <div ref={root} className="relative">
      <button
        onClick={() => setOpen((o) => !o)}
        className="inline-flex h-8 max-w-[34rem] items-center gap-2 rounded-full bg-white/[0.04] px-3 text-xs text-slate-200 ring-1 ring-line hover:bg-white/[0.07]"
      >
        <Sparkles className="size-3.5 shrink-0 text-emerald-300" />
        <span className="shrink-0 font-semibold">{backend === "hermes" ? "Hermes" : "Claude Code"}</span>
        <span className="text-slate-600">/</span>
        <span className="truncate text-slate-300">{label}</span>
        <ChevronDown className="size-3.5 shrink-0 text-slate-500" />
      </button>
      {open && (
        <div className="absolute left-0 top-10 z-50 flex w-[44rem] overflow-hidden rounded-2xl border border-line bg-card shadow-2xl">
          <div className="w-56 shrink-0 border-r border-line p-2">
            <div className="px-2 pb-1.5 pt-1 text-caption font-semibold uppercase tracking-wide text-slate-500">Provider</div>
            {!cfg && <Loader2 className="m-3 size-4 animate-spin text-slate-500" />}
            {cfg?.providers.map((p) => (
              <button
                key={p.id}
                onClick={() => setProvider(p.id)}
                className={`flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-xs ${provider === p.id ? "bg-white/[0.07] text-slate-100" : "text-slate-400 hover:bg-white/[0.03]"}`}
              >
                <span className={`size-1.5 shrink-0 rounded-full ${p.ready ? "bg-emerald-400" : "bg-slate-600"}`} />
                <span className="min-w-0 flex-1 truncate">{p.label}</span>
                {p.active && <Check className="size-3.5 text-emerald-300" />}
              </button>
            ))}
            <button onClick={onOpenControl} className="mt-2 w-full rounded-lg px-2 py-1.5 text-left text-label text-emerald-300 hover:bg-white/[0.03]">
              Keys and sign-in in Control Plane
            </button>
          </div>
          <div className="flex min-w-0 flex-1 flex-col">
            <div className="border-b border-line p-2">
              {current && !current.ready && (
                <div className="mb-2 rounded-lg bg-amber-500/10 px-2.5 py-1.5 text-label text-amber-200 ring-1 ring-amber-500/25">{current.detail}</div>
              )}
              <div className="flex items-center gap-2 rounded-lg bg-canvas px-2 ring-1 ring-line">
                <Search className="size-3.5 text-slate-500" />
                <input
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                  placeholder={`Filter ${models.length} models or type any model id`}
                  className="h-8 min-w-0 flex-1 bg-transparent text-xs text-slate-100 outline-none placeholder:text-slate-600"
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && filter.trim()) void choose(filter.trim());
                  }}
                />
              </div>
            </div>
            <div className="max-h-72 min-h-40 overflow-y-auto p-1.5">
              {loadingModels && <Loader2 className="m-3 size-4 animate-spin text-slate-500" />}
              {!loadingModels && shown.length === 0 && (
                <div className="p-3 text-xs text-slate-500">{filter ? "Press Enter to use this exact model id." : "No models found for this provider."}</div>
              )}
              {shown.map((m) => {
                const active = current?.active && cfg?.active.model === m;
                return (
                  <button
                    key={m}
                    disabled={busy !== null}
                    onClick={() => void choose(m)}
                    className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left font-mono text-small ${active ? "bg-emerald-500/10 text-emerald-200" : "text-slate-300 hover:bg-white/[0.04]"}`}
                  >
                    <span className="min-w-0 flex-1 truncate">{m}</span>
                    {busy === m && <Loader2 className="size-3.5 animate-spin" />}
                    {active && <Check className="size-3.5" />}
                  </button>
                );
              })}
            </div>
            <div className="border-t border-line p-2">
              <div className="px-1 pb-1.5 text-caption font-semibold uppercase tracking-wide text-slate-500">MCP servers for Hermes</div>
              <div className="flex flex-wrap gap-x-4 gap-y-1.5 px-1">
                {servers.map((s) => {
                  const reg = s.agents.find((a) => a.agent === "hermes");
                  return (
                    <label key={s.name} className="inline-flex items-center gap-2 text-xs text-slate-300">
                      <Plug className="size-3 text-violet-300" />
                      {s.name}
                      {busy === s.name ? <Loader2 className="size-3.5 animate-spin" /> : <Toggle checked={Boolean(reg?.enabled)} onChange={(v) => void toggleServer(s.name, v)} label={`Enable ${s.name}`} />}
                    </label>
                  );
                })}
                {servers.length === 0 && <span className="text-label text-slate-500">No MCP servers in Hermes yet</span>}
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
