import {
  Activity,
  Bot,
  CheckCircle2,
  CircleAlert,
  GitBranch,
  KeyRound,
  LibraryBig,
  Link2,
  Loader2,
  LogIn,
  Plug,
  Plus,
  Power,
  Search,
  Trash2,
  Wand2,
  X,
  Zap,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import {
  MCP_AGENTS,
  api,
  errorText,
  type AgentOutcome,
  type CatalogEntry,
  type McpServer,
  type ProbeResult,
  type ProviderConfig,
  type ProviderTest,
  type ProviderView,
} from "../lib/api";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, Field, Modal, Toggle, inputClass } from "../components/ui";

type Tab = "providers" | "mcp";

const AGENT_LABEL: Record<string, string> = { hermes: "Hermes", "claude-code": "Claude Code", qoder: "Qoder", antigravity: "Antigravity" };
const AGENT_SHORT: Record<string, string> = { hermes: "H", "claude-code": "C", qoder: "Q", antigravity: "A" };

export function ControlPlane({ tab, onTab, onStatusChanged }: { tab: Tab; onTab: (t: Tab) => void; onStatusChanged: () => void }) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-end justify-between px-6 pt-5">
        <div>
          <h1 className="text-lg font-semibold text-slate-100">MCP and Provider Switchboard</h1>
          <p className="text-xs text-muted">Pick the model behind Hermes in two clicks, and manage every MCP server across Hermes, Claude Code, Qoder and Antigravity.</p>
        </div>
        <div className="flex rounded-lg bg-white/[0.04] p-0.5 ring-1 ring-line">
          {(
            [
              ["providers", "Providers", <Bot key="b" className="size-3.5" />],
              ["mcp", "MCP Server Hub", <Plug key="p" className="size-3.5" />],
            ] as [Tab, string, ReactNode][]
          ).map(([id, label, icon]) => (
            <button
              key={id}
              onClick={() => onTab(id)}
              className={`inline-flex h-8 items-center gap-1.5 rounded-md px-3 text-xs font-medium ${tab === id ? "bg-emerald-500/15 text-emerald-300" : "text-slate-400 hover:text-slate-200"}`}
            >
              {icon}
              {label}
            </button>
          ))}
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-8 pt-4">{tab === "providers" ? <Providers onStatusChanged={onStatusChanged} /> : <McpHub onStatusChanged={onStatusChanged} />}</div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------------------------

function Providers({ onStatusChanged }: { onStatusChanged: () => void }) {
  const toast = useToast();
  const [cfg, setCfg] = useState<ProviderConfig | null>(null);
  const load = useCallback(async () => {
    try {
      setCfg(await api.providerConfig());
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);
  useEffect(() => { void load(); }, [load]);
  if (!cfg) return <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />;
  return (
    <>
      <Card className="mb-4 flex items-center gap-4 px-5 py-4">
        <div className="grid size-10 place-items-center rounded-xl bg-emerald-500/10 ring-1 ring-emerald-500/25">
          <Zap className="size-5 text-emerald-300" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="text-label uppercase tracking-wide text-slate-500">Hermes is running on</div>
          <div className="truncate text-sm font-semibold text-slate-100">
            {cfg.active_label} <span className="font-mono text-xs font-normal text-slate-400">{cfg.active.model}</span>
          </div>
        </div>
        <div className="text-right text-label leading-relaxed text-slate-500">
          <div title={cfg.config_path}>Settings: config.yaml</div>
          <div title={cfg.env_path}>Keys: Hermes .env (names only shown here)</div>
        </div>
      </Card>
      <div className="grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3">
        {cfg.providers.map((p) => (
          <ProviderCard
            key={p.id}
            p={p}
            onChanged={(c) => {
              setCfg(c);
              onStatusChanged();
            }}
            onReload={() => void load()}
          />
        ))}
      </div>
    </>
  );
}

function ProviderCard({ p, onChanged, onReload }: { p: ProviderView; onChanged: (c: ProviderConfig) => void; onReload: () => void }) {
  const toast = useToast();
  const [model, setModel] = useState(p.default_model);
  const [models, setModels] = useState<string[] | null>(null);
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(p.kind === "local" ? "http://localhost:11434/v1" : "");
  const [busy, setBusy] = useState<"activate" | "test" | "login" | null>(null);
  const [test, setTest] = useState<ProviderTest | null>(null);

  useEffect(() => { setModel(p.default_model); }, [p.default_model]);

  async function loadModels() {
    if (models) return;
    try {
      const m = await api.models(p.id);
      setModels(m);
      if (!model && m[0]) setModel(m[0]);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function activate() {
    setBusy("activate");
    try {
      const r = await api.setProvider(p.id, model, key.trim() || null, p.kind === "local" ? baseUrl : null);
      setKey("");
      onChanged(r.config);
      toast("ok", r.log.join(". "));
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function runTest() {
    setBusy("test");
    setTest(null);
    try {
      if (key.trim()) {
        const r = await api.setProvider(p.id, model, key.trim(), p.kind === "local" ? baseUrl : null);
        setKey("");
        onChanged(r.config);
      }
      setTest(await api.testProvider(p.id, model, p.kind === "local" ? baseUrl : null));
    } catch (e) {
      setTest({ ok: false, latency_ms: 0, reply: "", error: errorText(e) });
    } finally {
      setBusy(null);
    }
  }

  async function login() {
    setBusy("login");
    try {
      toast("info", await api.loginProvider(p.id));
      window.setTimeout(onReload, 15000);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const listId = `models-${p.id}`;
  return (
    <Card className={`flex flex-col p-4 ${p.active ? "ring-1 ring-emerald-500/40" : ""}`}>
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-semibold text-slate-100">{p.label}</span>
            {p.active && <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/30">Active</Badge>}
          </div>
          <p className="mt-0.5 text-small leading-relaxed text-slate-500">{p.blurb}</p>
        </div>
        <span className={`mt-1 inline-flex items-center gap-1 text-label ${p.ready ? "text-emerald-300" : "text-amber-300"}`}>
          {p.ready ? <CheckCircle2 className="size-3.5" /> : <CircleAlert className="size-3.5" />}
          {p.ready ? "Ready" : "Setup"}
        </span>
      </div>
      <div className="mt-1 text-label text-slate-400">{p.detail}</div>
      <div className="mt-3 space-y-2">
        <div>
          <input
            list={listId}
            value={model}
            onFocus={() => void loadModels()}
            onChange={(e) => setModel(e.target.value)}
            placeholder={p.kind === "local" ? "llama3.3:70b" : "model id"}
            className={`${inputClass} font-mono text-xs`}
            aria-label={`${p.label} model`}
          />
          <datalist id={listId}>{models?.slice(0, 300).map((m) => <option key={m} value={m} />)}</datalist>
        </div>
        {p.kind === "api_key" && (
          <div className="flex items-center gap-2">
            <KeyRound className="size-3.5 shrink-0 text-slate-500" />
            <input
              type="password"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder={p.key_set ? `${p.key_env[0]} is stored (paste to replace)` : `Paste ${p.key_env[0]}`}
              className={`${inputClass} text-xs`}
              autoComplete="off"
              aria-label={`${p.label} API key`}
            />
          </div>
        )}
        {p.kind === "local" && <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} className={`${inputClass} font-mono text-xs`} aria-label="Base URL" />}
      </div>
      {test && (
        <div className={`mt-3 rounded-lg px-3 py-2 text-small ring-1 ${test.ok ? "bg-emerald-500/10 text-emerald-200 ring-emerald-500/25" : "bg-red-500/10 text-red-200 ring-red-500/25"}`}>
          {test.ok ? `Connected in ${(test.latency_ms / 1000).toFixed(1)}s. Reply: ${test.reply}` : `Failed: ${test.error || test.reply || "no reply"}`}
        </div>
      )}
      <div className="mt-auto flex flex-wrap gap-2 pt-3">
        <Button size="sm" variant={p.active ? "secondary" : "primary"} busy={busy === "activate"} disabled={!model.trim() || busy !== null} onClick={() => void activate()} icon={<Power className="size-3.5" />}>
          {p.active ? "Re-apply" : "Activate Provider"}
        </Button>
        <Button size="sm" busy={busy === "test"} disabled={!model.trim() || busy !== null} onClick={() => void runTest()} icon={<Activity className="size-3.5" />}>
          Test connection
        </Button>
        {(p.kind === "subscription" || p.kind === "oauth") && (
          <Button size="sm" variant="ghost" busy={busy === "login"} disabled={busy !== null} onClick={() => void login()} icon={<LogIn className="size-3.5" />}>
            Sign in
          </Button>
        )}
      </div>
    </Card>
  );
}

// ---------------------------------------------------------------------------------------------
// MCP Server Hub
// ---------------------------------------------------------------------------------------------

function outcomesText(o: AgentOutcome[]) {
  return o.map((x) => `${AGENT_LABEL[x.agent] ?? x.agent}: ${x.ok ? "ok" : x.detail}`).join(" | ");
}

function McpHub({ onStatusChanged }: { onStatusChanged: () => void }) {
  const toast = useToast();
  const [servers, setServers] = useState<McpServer[] | null>(null);
  const [probes, setProbes] = useState<Record<string, ProbeResult | "running">>({});
  const [inspect, setInspect] = useState<string | null>(null);
  const [catalogOpen, setCatalogOpen] = useState(false);
  const [customOpen, setCustomOpen] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setServers(await api.mcpServers());
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  const probe = useCallback(async (name: string) => {
    setProbes((p) => ({ ...p, [name]: "running" }));
    try {
      const r = await api.testMcpServer(name);
      setProbes((p) => ({ ...p, [name]: r }));
      return r;
    } catch (e) {
      const r: ProbeResult = { name, ok: false, via: "native", latency_ms: 0, server_name: "", server_version: "", protocol: "", tools: [], error: errorText(e) };
      setProbes((p) => ({ ...p, [name]: r }));
      return r;
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const serverNames = servers?.map((s) => s.name).join("|") ?? "";
  useEffect(() => {
    if (serverNames) serverNames.split("|").forEach((n) => void probe(n));
  }, [serverNames, probe]);

  async function act(key: string, fn: () => Promise<string>) {
    setBusy(key);
    try {
      toast("ok", await fn());
      await load();
      onStatusChanged();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  const needsCutover = servers?.some((s) => s.is_agentos_hub && (s.hub_outdated || s.legacy_python || s.agents.length < 4 || s.agents.some((a) => !a.matches_primary))) ?? false;
  const hubMissing = servers !== null && !servers.some((s) => s.is_agentos_hub);

  return (
    <>
      <div className="mb-4 flex flex-wrap items-center gap-2">
        <Button variant="primary" onClick={() => setCatalogOpen(true)} icon={<LibraryBig className="size-4" />}>
          Add from Catalog
        </Button>
        <Button onClick={() => setCustomOpen(true)} icon={<Plus className="size-4" />}>
          Add Custom Server
        </Button>
        <Button onClick={() => servers?.forEach((s) => void probe(s.name))} icon={<Activity className="size-4" />}>
          Test All
        </Button>
        {(needsCutover || hubMissing) && (
          <Button
            variant="secondary"
            busy={busy === "cutover"}
            onClick={() => void act("cutover", async () => outcomesText(await api.cutoverHub()))}
            icon={<Wand2 className="size-4 text-emerald-300" />}
            className="ring-emerald-500/40"
          >
            {hubMissing ? "Register agentos-hub in every agent" : "Update agentos-hub in every agent to this version"}
          </Button>
        )}
      </div>
      {!servers && <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />}
      <div className="space-y-2">
        {servers?.map((s) => {
          const pr = probes[s.name];
          const missing = MCP_AGENTS.filter((a) => !s.agents.some((x) => x.agent === a));
          return (
            <Card key={s.name} className="flex items-center gap-4 px-4 py-3">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-semibold text-slate-100">{s.name}</span>
                  <Badge className="bg-white/5 text-slate-400 ring-white/10">{s.transport}</Badge>
                  {s.auth && <Badge className="bg-sky-500/10 text-sky-300 ring-sky-500/25">{s.auth}</Badge>}
                  {s.is_agentos_hub && !s.legacy_python && <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/25">Rust</Badge>}
                  {s.legacy_python && <Badge className="bg-amber-500/10 text-amber-300 ring-amber-500/25">Python somewhere</Badge>}
                </div>
                <div className="mt-0.5 truncate font-mono text-label text-slate-500" title={s.url ?? [s.command, ...s.args].join(" ")}>
                  {s.url ?? [s.command, ...s.args].join(" ")}
                  {s.env_keys.length > 0 && <span className="ml-2 text-slate-600">env: {s.env_keys.join(", ")}</span>}
                </div>
              </div>
              <div className="flex shrink-0 items-center gap-1" aria-label="Agents">
                {MCP_AGENTS.map((a) => {
                  const reg = s.agents.find((x) => x.agent === a);
                  const tone = !reg
                    ? "bg-white/[0.02] text-slate-600 ring-white/5 hover:text-slate-300 hover:ring-slate-500"
                    : !reg.enabled
                      ? "bg-slate-500/10 text-slate-400 ring-slate-500/30"
                      : reg.matches_primary
                        ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/30"
                        : "bg-amber-500/10 text-amber-300 ring-amber-500/30";
                  const title = !reg
                    ? `Not in ${AGENT_LABEL[a]}: click to add it there`
                    : `${AGENT_LABEL[a]}: ${reg.enabled ? "enabled" : "disabled"}${reg.matches_primary ? "" : ", points somewhere else"}`;
                  return (
                    <button
                      key={a}
                      title={title}
                      disabled={Boolean(reg) || busy !== null}
                      onClick={() => void act(`sync-${s.name}`, async () => outcomesText(await api.syncMcpServer(s.name, [a])))}
                      className={`grid size-7 place-items-center rounded-md text-label font-semibold ring-1 ${tone}`}
                    >
                      {AGENT_SHORT[a]}
                    </button>
                  );
                })}
                {missing.length > 0 && (
                  <button
                    title={`Add to ${missing.map((m) => AGENT_LABEL[m]).join(", ")}`}
                    disabled={busy !== null}
                    onClick={() => void act(`sync-${s.name}`, async () => outcomesText(await api.syncMcpServer(s.name, missing)))}
                    className="ml-1 rounded-md px-1.5 py-1 text-label text-emerald-300 hover:bg-emerald-500/10"
                  >
                    {busy === `sync-${s.name}` ? <Loader2 className="size-3.5 animate-spin" /> : "Sync all"}
                  </button>
                )}
              </div>
              <div className="w-36 shrink-0 text-right text-small">
                {pr === "running" || pr === undefined ? (
                  <span className="inline-flex items-center gap-1 text-slate-500">
                    <Loader2 className="size-3 animate-spin" /> testing
                  </span>
                ) : pr.ok ? (
                  <span className="text-emerald-300">
                    {pr.latency_ms} ms, {pr.tools.length} tools
                  </span>
                ) : (
                  <span className="text-red-300" title={pr.error ?? ""}>
                    unreachable
                  </span>
                )}
              </div>
              <div className="flex shrink-0 items-center gap-1">
                {s.agents.some((a) => a.agent === "hermes") && (
                  <Toggle
                    checked={s.agents.find((a) => a.agent === "hermes")?.enabled ?? false}
                    onChange={(v) => void act(`en-${s.name}`, () => api.setMcpEnabled(s.name, v))}
                    label={`Enable ${s.name} in Hermes`}
                  />
                )}
                <Button size="sm" variant="ghost" onClick={() => setInspect(s.name)} icon={<Search className="size-3.5" />}>
                  Test and Inspect
                </Button>
                {s.transport === "http" && (
                  <Button size="sm" variant="ghost" onClick={() => void act(`oauth-${s.name}`, () => api.mcpOauth(s.name))} icon={<Link2 className="size-3.5" />}>
                    Sign in
                  </Button>
                )}
                <Button
                  size="sm"
                  variant={confirmRemove === s.name ? "danger" : "ghost"}
                  busy={busy === `rm-${s.name}`}
                  onClick={() => {
                    if (confirmRemove !== s.name) {
                      setConfirmRemove(s.name);
                      window.setTimeout(() => setConfirmRemove((c) => (c === s.name ? null : c)), 4000);
                      return;
                    }
                    setConfirmRemove(null);
                    void act(`rm-${s.name}`, async () => outcomesText(await api.removeMcpServer(s.name)));
                  }}
                  icon={<Trash2 className="size-3.5" />}
                >
                  {confirmRemove === s.name ? "Remove everywhere" : ""}
                </Button>
              </div>
            </Card>
          );
        })}
      </div>
      {inspect && (
        <InspectDrawer
          name={inspect}
          result={probes[inspect]}
          onRetest={() => void probe(inspect)}
          onClose={() => setInspect(null)}
        />
      )}
      {catalogOpen && (
        <CatalogModal
          installed={new Set(servers?.map((s) => s.name) ?? [])}
          onClose={() => setCatalogOpen(false)}
          onInstalled={() => {
            void load();
            onStatusChanged();
          }}
        />
      )}
      {customOpen && (
        <CustomServerModal
          onClose={() => setCustomOpen(false)}
          onAdded={() => {
            void load();
            onStatusChanged();
          }}
        />
      )}
    </>
  );
}

function InspectDrawer({ name, result, onRetest, onClose }: { name: string; result: ProbeResult | "running" | undefined; onRetest: () => void; onClose: () => void }) {
  const [filter, setFilter] = useState("");
  const tools = result && result !== "running" ? result.tools.filter((t) => `${t.name} ${t.description}`.toLowerCase().includes(filter.toLowerCase())) : [];
  return (
    <div className="fixed inset-0 z-50 flex justify-end bg-black/40" onMouseDown={onClose}>
      <div className="animate-slide-in flex h-full w-[38rem] flex-col border-l border-line bg-card shadow-2xl" onMouseDown={(e) => e.stopPropagation()}>
        <div className="flex items-center gap-3 border-b border-line px-5 py-4">
          <Plug className="size-4 text-violet-300" />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-semibold text-slate-100">{name}</div>
            {result && result !== "running" && (
              <div className="text-label text-slate-500">
                {result.ok
                  ? `${result.server_name || name} ${result.server_version} - protocol ${result.protocol || "n/a"} - ${result.latency_ms} ms via ${result.via === "native" ? "native Rust client" : "Hermes"}`
                  : result.error}
              </div>
            )}
          </div>
          <Button size="sm" onClick={onRetest} busy={result === "running"} icon={<Activity className="size-3.5" />}>
            Test again
          </Button>
          <button onClick={onClose} className="rounded-md p-1 text-slate-500 hover:bg-white/5 hover:text-slate-200" aria-label="Close">
            <X className="size-4" />
          </button>
        </div>
        <div className="border-b border-line px-5 py-2">
          <input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Filter tools" className={`${inputClass} text-xs`} />
        </div>
        <div className="min-h-0 flex-1 space-y-2 overflow-y-auto px-5 py-3">
          {result === "running" && <Loader2 className="mx-auto mt-8 size-5 animate-spin text-slate-500" />}
          {result && result !== "running" && !result.ok && <div className="rounded-lg bg-red-500/10 p-3 text-xs text-red-200 ring-1 ring-red-500/25">{result.error}</div>}
          {tools.map((t) => (
            <details key={t.name} className="rounded-lg border border-line bg-canvas/60">
              <summary className="cursor-pointer px-3 py-2">
                <span className="font-mono text-xs text-slate-100">{t.name}</span>
                <div className="mt-0.5 text-label leading-relaxed text-slate-500">{t.description.slice(0, 220)}</div>
              </summary>
              <pre className="max-h-80 overflow-auto border-t border-line p-3 font-mono text-label text-slate-300">
                {t.input_schema ? JSON.stringify(t.input_schema, null, 2) : "Schema not available for servers tested through Hermes"}
              </pre>
            </details>
          ))}
        </div>
      </div>
    </div>
  );
}

function CatalogModal({ installed, onClose, onInstalled }: { installed: Set<string>; onClose: () => void; onInstalled: () => void }) {
  const toast = useToast();
  const [catalog, setCatalog] = useState<CatalogEntry[] | null>(null);
  const [pick, setPick] = useState<CatalogEntry | null>(null);
  const [values, setValues] = useState<Record<string, string>>({});
  const [all, setAll] = useState(true);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<AgentOutcome[] | null>(null);

  useEffect(() => {
    api.mcpCatalog().then(setCatalog).catch((e) => toast("error", errorText(e)));
  }, [toast]);

  function choose(c: CatalogEntry) {
    setPick(c);
    setResult(null);
    setValues(Object.fromEntries(c.params.map((p) => [p.key, p.default])));
  }

  async function install() {
    if (!pick) return;
    setBusy(true);
    try {
      const r = await api.installCatalog(pick.id, values, all);
      setResult(r);
      onInstalled();
      if (r.every((x) => x.ok)) toast("ok", `${pick.name} installed`);
      if (pick.auth === "oauth" && r.some((x) => x.agent === "hermes" && x.ok)) {
        toast("info", await api.mcpOauth(pick.id));
      }
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function fillGh(key: string) {
    try {
      const t = await api.ghToken();
      setValues((v) => ({ ...v, [key]: t }));
      toast("ok", "Filled from your GitHub CLI login");
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  return (
    <Modal
      title={pick ? `Add ${pick.name}` : "MCP Catalog"}
      onClose={onClose}
      footer={
        pick ? (
          <>
            <Button variant="ghost" onClick={() => setPick(null)}>
              Back
            </Button>
            <Button variant="primary" busy={busy} onClick={() => void install()} icon={<Plus className="size-4" />}>
              {installed.has(pick.id) ? "Reinstall" : "Install"}
            </Button>
          </>
        ) : undefined
      }
    >
      {!catalog && <Loader2 className="mx-auto size-5 animate-spin text-slate-500" />}
      {catalog && !pick && (
        <div className="grid max-h-[60vh] grid-cols-1 gap-2 overflow-y-auto">
          {catalog.map((c) => (
            <button key={c.id} onClick={() => choose(c)} className="rounded-xl border border-line bg-canvas/50 px-3.5 py-3 text-left hover:border-slate-600">
              <div className="flex items-center gap-2">
                <span className="text-sm font-semibold text-slate-100">{c.name}</span>
                <Badge className="bg-white/5 text-slate-400 ring-white/10">{c.transport}</Badge>
                {installed.has(c.id) && <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/25">installed</Badge>}
              </div>
              <div className="mt-0.5 text-small text-slate-400">{c.description}</div>
              <div className="mt-1 text-caption text-slate-600">Needs: {c.requires}</div>
            </button>
          ))}
        </div>
      )}
      {pick && (
        <div className="space-y-3">
          <p className="text-xs text-slate-400">{pick.description}</p>
          <div className="rounded-lg bg-canvas p-2.5 font-mono text-label text-slate-400">{[pick.command_or_url, ...pick.args].join(" ")}</div>
          {pick.params.map((p) => (
            <Field key={p.key} label={p.label} hint={p.env ? `Stored as ${p.key} in each agent's server config` : undefined}>
              <div className="flex gap-2">
                <input
                  type={p.secret ? "password" : "text"}
                  className={`${inputClass} text-xs`}
                  placeholder={p.placeholder}
                  value={values[p.key] ?? ""}
                  onChange={(e) => setValues((v) => ({ ...v, [p.key]: e.target.value }))}
                  autoComplete="off"
                />
                {p.key === "GITHUB_PERSONAL_ACCESS_TOKEN" && (
                  <Button size="md" onClick={() => void fillGh(p.key)} icon={<GitBranch className="size-4" />}>
                    Use gh login
                  </Button>
                )}
              </div>
            </Field>
          ))}
          <label className="flex items-center gap-2 text-xs text-slate-300">
            <Toggle checked={all} onChange={setAll} label="Sync to all agents" />
            Add to Hermes, Claude Code, Qoder and Antigravity (off: Hermes only)
          </label>
          {result && (
            <div className="space-y-1">
              {result.map((r) => (
                <div key={r.agent} className={`text-small ${r.ok ? "text-emerald-300" : "text-red-300"}`}>
                  {AGENT_LABEL[r.agent]}: {r.ok ? "added" : r.detail}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </Modal>
  );
}

function CustomServerModal({ onClose, onAdded }: { onClose: () => void; onAdded: () => void }) {
  const toast = useToast();
  const [name, setName] = useState("");
  const [transport, setTransport] = useState<"stdio" | "http">("stdio");
  const [target, setTarget] = useState("");
  const [args, setArgs] = useState("");
  const [env, setEnv] = useState("");
  const [oauth, setOauth] = useState(false);
  const [agents, setAgents] = useState<string[]>([...MCP_AGENTS]);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<AgentOutcome[] | null>(null);

  const envVars = useMemo(() => {
    const out: Record<string, string> = {};
    for (const line of env.split("\n")) {
      const i = line.indexOf("=");
      if (i > 0) out[line.slice(0, i).trim()] = line.slice(i + 1).trim();
    }
    return out;
  }, [env]);

  async function add() {
    setBusy(true);
    try {
      const r = await api.addMcpServer({
        name: name.trim(),
        transport,
        commandOrUrl: target.trim(),
        args: args
          .split("\n")
          .map((a) => a.trim())
          .filter(Boolean),
        envVars,
        syncToAllAgents: agents.length === MCP_AGENTS.length,
        auth: transport === "http" && oauth ? "oauth" : null,
        agents,
      });
      setResult(r);
      onAdded();
      if (r.every((x) => x.ok)) toast("ok", `${name} added`);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      title="Add a custom MCP server"
      onClose={onClose}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Close
          </Button>
          <Button variant="primary" busy={busy} disabled={!name.trim() || !target.trim() || agents.length === 0} onClick={() => void add()} icon={<Plus className="size-4" />}>
            Add server
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="grid grid-cols-[1fr_auto] gap-2">
          <Field label="Name">
            <input className={`${inputClass} text-xs`} value={name} onChange={(e) => setName(e.target.value)} placeholder="my-server" />
          </Field>
          <Field label="Transport">
            <select className={`${inputClass} text-xs`} value={transport} onChange={(e) => setTransport(e.target.value as "stdio" | "http")}>
              <option value="stdio">stdio</option>
              <option value="http">http</option>
            </select>
          </Field>
        </div>
        <Field label={transport === "stdio" ? "Command" : "URL"}>
          <input className={`${inputClass} font-mono text-xs`} value={target} onChange={(e) => setTarget(e.target.value)} placeholder={transport === "stdio" ? "npx" : "https://example.com/mcp"} />
        </Field>
        {transport === "stdio" ? (
          <>
            <Field label="Arguments" hint="One per line">
              <textarea rows={3} className={`${inputClass} font-mono text-xs`} value={args} onChange={(e) => setArgs(e.target.value)} placeholder={"-y\n@scope/server"} />
            </Field>
            <Field label="Environment" hint="KEY=VALUE, one per line">
              <textarea rows={2} className={`${inputClass} font-mono text-xs`} value={env} onChange={(e) => setEnv(e.target.value)} placeholder="API_KEY=..." />
            </Field>
          </>
        ) : (
          <label className="flex items-center gap-2 text-xs text-slate-300">
            <Toggle checked={oauth} onChange={setOauth} label="OAuth" /> Uses OAuth sign-in
          </label>
        )}
        <div className="flex flex-wrap gap-3">
          {MCP_AGENTS.map((a) => (
            <label key={a} className="inline-flex items-center gap-1.5 text-xs text-slate-300">
              <input
                type="checkbox"
                checked={agents.includes(a)}
                onChange={(e) => setAgents((cur) => (e.target.checked ? [...cur, a] : cur.filter((x) => x !== a)))}
                className="accent-emerald-500"
              />
              {AGENT_LABEL[a]}
            </label>
          ))}
        </div>
        {result && (
          <div className="space-y-1">
            {result.map((r) => (
              <div key={r.agent} className={`text-small ${r.ok ? "text-emerald-300" : "text-red-300"}`}>
                {AGENT_LABEL[r.agent]}: {r.detail}
              </div>
            ))}
          </div>
        )}
      </div>
    </Modal>
  );
}
