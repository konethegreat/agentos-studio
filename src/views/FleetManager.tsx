import { open } from "@tauri-apps/plugin-dialog";
import { CheckCircle2, CircleAlert, FolderGit2, FolderOpen, GitBranch, Plus, RefreshCw, Search, Send } from "lucide-react";
import { useMemo, useState } from "react";
import { api, errorText, type RepoInfo, type SyncResult } from "../lib/api";
import { Badge, Button, Card, Field, Modal, Toggle, inputClass } from "../components/ui";
import { useToast } from "../components/Toasts";

const FILTERS = ["all", "active", "recent", "dormant"] as const;
type Filter = (typeof FILTERS)[number];

const STATUS_COLOR: Record<string, string> = {
  active: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25",
  recent: "bg-sky-500/10 text-sky-300 ring-sky-500/25",
  dormant: "bg-slate-500/10 text-slate-400 ring-slate-500/25",
  "no commits": "bg-slate-500/10 text-slate-500 ring-slate-500/25",
};

export function FleetManager({ repos, scanning, onRescan }: { repos: RepoInfo[]; scanning: boolean; onRescan: () => Promise<void> }) {
  const toast = useToast();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [autoCommit, setAutoCommit] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [results, setResults] = useState<SyncResult[] | null>(null);
  const [showNew, setShowNew] = useState(false);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    return repos.filter(
      (r) =>
        (filter === "all" || r.status === filter) &&
        (!q || r.name.toLowerCase().includes(q) || r.path.toLowerCase().includes(q) || r.stack.join(" ").toLowerCase().includes(q)),
    );
  }, [repos, query, filter]);

  const synced = repos.filter((r) => r.rules_synced).length;

  async function sync(paths: string[]) {
    if (paths.length === 0) return;
    setSyncing(true);
    try {
      const res = await api.syncFleet(paths, autoCommit);
      setResults(res);
      const errors = res.filter((r) => r.error).length;
      toast(errors ? "error" : "ok", `Constitution synced to ${res.length - errors}/${res.length} repositories${autoCommit ? " and rule files committed" : ""}`);
      await onRescan();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setSyncing(false);
    }
  }

  function toggle(path: string) {
    setSelected((s) => {
      const n = new Set(s);
      if (n.has(path)) n.delete(path);
      else n.add(path);
      return n;
    });
  }

  return (
    <div className="h-full overflow-y-auto px-6 pb-10 pt-5">
      <div className="mb-5 flex items-end justify-between gap-4">
        <div>
          <h1 className="text-lg font-semibold text-slate-100">Fleet Repositories</h1>
          <p className="text-xs text-muted">
            {repos.length} local repositories found, {synced} on the current constitution.
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button variant="secondary" busy={scanning} icon={<RefreshCw className="size-4" />} onClick={onRescan}>
            Rescan
          </Button>
          <Button variant="secondary" icon={<Plus className="size-4" />} onClick={() => setShowNew(true)}>
            New AgentOS Project
          </Button>
          <Button variant="primary" busy={syncing} icon={<Send className="size-4" />} onClick={() => sync(selected.size ? [...selected] : repos.map((r) => r.path))}>
            {selected.size ? `Sync Constitution to ${selected.size} Selected` : "Sync Constitution to All Repos"}
          </Button>
        </div>
      </div>

      <div className="mb-4 flex items-center gap-3">
        <div className="flex flex-1 items-center gap-2 rounded-lg border border-line bg-card px-3">
          <Search className="size-4 text-slate-500" />
          <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search by name, path or stack" className="h-9 flex-1 bg-transparent text-sm outline-none placeholder:text-slate-600" />
        </div>
        <div className="flex rounded-lg border border-line bg-card p-1">
          {FILTERS.map((f) => (
            <button key={f} onClick={() => setFilter(f)} className={`h-7 rounded-md px-3 text-xs capitalize ${filter === f ? "bg-white/10 text-slate-100" : "text-slate-500 hover:text-slate-300"}`}>
              {f}
            </button>
          ))}
        </div>
        <label className="flex items-center gap-2 text-xs text-slate-400">
          <Toggle checked={autoCommit} onChange={setAutoCommit} label="Commit rule files" />
          Commit rule files only
        </label>
      </div>

      <Card className="overflow-hidden">
        <table className="w-full text-sm">
          <thead className="border-b border-line bg-white/[0.02] text-left text-[11px] uppercase tracking-wide text-slate-500">
            <tr>
              <th className="w-10 px-4 py-2.5">
                <input
                  type="checkbox"
                  className="accent-emerald-500"
                  checked={visible.length > 0 && visible.every((r) => selected.has(r.path))}
                  onChange={(e) => setSelected(e.target.checked ? new Set(visible.map((r) => r.path)) : new Set())}
                />
              </th>
              <th className="px-2 py-2.5">Repository</th>
              <th className="px-2 py-2.5">Stack</th>
              <th className="px-2 py-2.5">Branch</th>
              <th className="px-2 py-2.5">Status</th>
              <th className="px-2 py-2.5 text-right">Uncommitted</th>
              <th className="px-4 py-2.5">Rules</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-line">
            {scanning && repos.length === 0 && (
              <tr>
                <td colSpan={7} className="px-4 py-10 text-center text-xs text-slate-500">
                  Scanning your folders for Git repositories...
                </td>
              </tr>
            )}
            {visible.map((r) => (
              <tr key={r.path} className="hover:bg-white/[0.02]">
                <td className="px-4 py-2.5">
                  <input type="checkbox" className="accent-emerald-500" checked={selected.has(r.path)} onChange={() => toggle(r.path)} />
                </td>
                <td className="px-2 py-2.5">
                  <div className="flex items-center gap-2">
                    <FolderGit2 className="size-4 text-slate-500" />
                    <div className="min-w-0">
                      <div className="font-medium text-slate-100">{r.name}</div>
                      <div className="max-w-80 truncate text-[11px] text-slate-500" title={r.path}>
                        {r.remote ? r.remote.replace("https://github.com/", "github: ").replace(/\.git$/, "") : r.path}
                      </div>
                    </div>
                  </div>
                </td>
                <td className="px-2 py-2.5">
                  <div className="flex max-w-72 flex-wrap gap-1">
                    {r.stack.slice(0, 4).map((s) => (
                      <Badge key={s} className="bg-white/5 text-slate-300 ring-white/10">
                        {s}
                      </Badge>
                    ))}
                    {r.stack.length > 4 && <span className="text-[11px] text-slate-500">+{r.stack.length - 4}</span>}
                  </div>
                </td>
                <td className="px-2 py-2.5">
                  <span className="inline-flex items-center gap-1 font-mono text-[11px] text-slate-400">
                    <GitBranch className="size-3" />
                    {r.branch}
                  </span>
                </td>
                <td className="px-2 py-2.5">
                  <Badge className={STATUS_COLOR[r.status]}>{r.status}</Badge>
                </td>
                <td className="px-2 py-2.5 text-right">
                  <span className={`font-mono text-xs ${r.dirty_files ? "text-amber-300" : "text-slate-600"}`}>{r.dirty_files}</span>
                </td>
                <td className="px-4 py-2.5">
                  {r.rules_synced ? (
                    <span className="inline-flex items-center gap-1 text-xs text-emerald-300">
                      <CheckCircle2 className="size-3.5" /> synced
                    </span>
                  ) : (
                    <button onClick={() => sync([r.path])} className="inline-flex items-center gap-1 text-xs text-amber-300 hover:text-amber-200">
                      <CircleAlert className="size-3.5" /> sync now
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </Card>

      {results && (
        <Card className="mt-4 p-4">
          <div className="mb-2 text-sm font-semibold text-slate-100">Last sync</div>
          <ul className="space-y-1 font-mono text-[11.5px]">
            {results.map((r) => (
              <li key={r.path} className={r.error ? "text-red-300" : "text-slate-400"}>
                {r.name}: {r.error ?? `${r.actions.filter(([, a]) => a !== "unchanged").length} files changed, ${r.commit}`}
                {r.skipped.length > 0 && <span className="text-amber-300"> (skipped {r.skipped.join("; ")})</span>}
              </li>
            ))}
          </ul>
        </Card>
      )}

      {showNew && <NewProjectModal onClose={() => setShowNew(false)} onCreated={onRescan} />}
    </div>
  );
}

function NewProjectModal({ onClose, onCreated }: { onClose: () => void; onCreated: () => Promise<void> }) {
  const toast = useToast();
  const [name, setName] = useState("");
  const [parent, setParent] = useState("");
  const [stack, setStack] = useState("rust");
  const [github, setGithub] = useState(true);
  const [priv, setPriv] = useState(true);
  const [busy, setBusy] = useState(false);
  const [steps, setSteps] = useState<string[]>([]);

  async function browse() {
    const picked = await open({ directory: true, title: "Parent folder for the new project" });
    if (typeof picked === "string") setParent(picked);
  }

  async function create() {
    setBusy(true);
    try {
      const r = await api.createProject(name.trim(), parent.trim(), stack, github, priv);
      setSteps(r.steps);
      toast("ok", `${name} created${r.github ? ` and pushed to ${r.github}` : ""}. Notion sync queued as ${r.notion_task.id}.`);
      await onCreated();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      title="New AgentOS Project"
      onClose={onClose}
      footer={
        steps.length ? (
          <Button variant="primary" onClick={onClose}>
            Done
          </Button>
        ) : (
          <>
            <Button variant="ghost" onClick={onClose}>
              Cancel
            </Button>
            <Button variant="primary" busy={busy} disabled={!name.trim() || !parent.trim()} onClick={create}>
              Create Project
            </Button>
          </>
        )
      }
    >
      {steps.length ? (
        <ul className="space-y-1.5 text-sm text-slate-300">
          {steps.map((s) => (
            <li key={s} className="flex items-start gap-2">
              <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-emerald-400" />
              <span className="break-all">{s}</span>
            </li>
          ))}
        </ul>
      ) : (
        <div className="space-y-4">
          <Field label="Project name">
            <input className={inputClass} value={name} onChange={(e) => setName(e.target.value)} placeholder="my-new-service" autoFocus />
          </Field>
          <Field label="Parent folder" hint="The project is created as a subfolder here.">
            <div className="flex gap-2">
              <input className={inputClass} value={parent} onChange={(e) => setParent(e.target.value)} placeholder="C:\\Users\\you\\code" />
              <Button variant="secondary" icon={<FolderOpen className="size-4" />} onClick={browse}>
                Browse
              </Button>
            </div>
          </Field>
          <Field label="Stack" hint="Law 4: Rust for systems and services, TypeScript for frontends, Python for AI and data.">
            <select className={inputClass} value={stack} onChange={(e) => setStack(e.target.value)}>
              <option value="rust">Rust (cargo)</option>
              <option value="typescript">TypeScript (Node)</option>
              <option value="python">Python (uv)</option>
              <option value="empty">Empty</option>
            </select>
          </Field>
          <div className="flex items-center justify-between rounded-lg border border-line px-3 py-2.5">
            <span className="text-sm text-slate-300">Create GitHub repository</span>
            <Toggle checked={github} onChange={setGithub} label="Create GitHub repository" />
          </div>
          {github && (
            <div className="flex items-center justify-between rounded-lg border border-line px-3 py-2.5">
              <span className="text-sm text-slate-300">{priv ? "Private repository" : "Public repository"}</span>
              <Toggle checked={priv} onChange={setPriv} label="Private repository" />
            </div>
          )}
        </div>
      )}
    </Modal>
  );
}
