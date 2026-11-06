import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  Archive,
  ArchiveRestore,
  ChevronRight,
  FolderPlus,
  GitBranch,
  Loader2,
  MessageSquarePlus,
  Pencil,
  Plus,
  RefreshCw,
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { api, errorText, type Project, type ProjectSession } from "../lib/api";
import { relativeTime } from "../lib/chat";
import { basename } from "../lib/format";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card, Empty, Field, Modal, inputClass } from "../components/ui";

/**
 * Projects as Hermes defines them: a named workspace with one or more folders, whose sessions
 * are the chats Hermes actually ran inside those folders. Studio adds branch links only.
 */
export function Projects({
  onOpenSession,
  onNewSession,
}: {
  onOpenSession: (session: ProjectSession) => void;
  onNewSession: (cwd: string, projectSlug: string | null, parent: string | null) => void;
}) {
  const toast = useToast();
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [sessions, setSessions] = useState<Record<string, ProjectSession[]>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [folders, setFolders] = useState<string[]>([]);
  const [showArchived, setShowArchived] = useState(false);

  const load = useCallback(async () => {
    try {
      setProjects(await api.projects());
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  useEffect(() => void load(), [load]);

  const loadSessions = useCallback(
    async (slug: string) => {
      setBusy(`s:${slug}`);
      try {
        const list = await api.projectSessions(slug);
        setSessions((prev) => ({ ...prev, [slug]: list }));
      } catch (e) {
        toast("error", errorText(e));
      } finally {
        setBusy(null);
      }
    },
    [toast],
  );

  async function toggle(p: Project) {
    if (open === p.slug) {
      setOpen(null);
      return;
    }
    setOpen(p.slug);
    if (!sessions[p.slug]) await loadSessions(p.slug);
  }

  async function act(key: string, fn: () => Promise<string>) {
    setBusy(key);
    try {
      toast("ok", await fn());
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function pickFolder() {
    const dir = await openDialog({ directory: true, title: "Folder for this project" });
    if (typeof dir === "string" && !folders.includes(dir)) setFolders([...folders, dir]);
  }

  async function create() {
    if (!name.trim() || folders.length === 0) return;
    setBusy("create");
    try {
      toast("ok", await api.createHermesProject(name.trim(), folders));
      setCreating(false);
      setName("");
      setFolders([]);
      await load();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function rename(p: Project) {
    const next = window.prompt("Project name", p.name);
    if (!next?.trim() || next === p.name) return;
    await act(`r:${p.slug}`, () => api.renameProject(p.slug, next.trim()));
  }

  const shown = (projects ?? []).filter((p) => showArchived || !p.archived);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-end justify-between px-6 pt-5">
        <div>
          <h1 className="text-lg font-semibold text-slate-100">Projects</h1>
          <p className="text-xs text-muted">
            Hermes projects: a named workspace over one or more folders, with every chat that ran inside them.
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant={showArchived ? "secondary" : "ghost"} onClick={() => setShowArchived((v) => !v)} icon={<Archive className="size-4" />}>
            {showArchived ? "Hiding nothing" : "Show archived"}
          </Button>
          <Button onClick={() => void load()} icon={<RefreshCw className="size-4" />}>
            Refresh
          </Button>
          <Button variant="primary" onClick={() => setCreating(true)} icon={<Plus className="size-4" />}>
            New project
          </Button>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-8 pt-4">
        {!projects && <Loader2 className="mx-auto mt-10 size-5 animate-spin text-slate-500" />}
        {projects && shown.length === 0 && (
          <Empty
            icon={<FolderPlus className="size-5" />}
            title="No projects yet"
            hint="Create one over a repository folder. Hermes then recognises every chat run inside it."
          />
        )}
        <div className="space-y-2">
          {shown.map((p) => {
            const expanded = open === p.slug;
            const list = sessions[p.slug] ?? [];
            return (
              <Card key={p.slug} className={p.archived ? "opacity-60" : ""}>
                <div className="flex items-center gap-3 px-4 py-3">
                  <button onClick={() => void toggle(p)} className="flex min-w-0 flex-1 items-center gap-2.5 text-left" aria-expanded={expanded}>
                    <ChevronRight className={`size-4 shrink-0 text-slate-500 transition-transform ${expanded ? "rotate-90" : ""}`} />
                    <div className="min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="truncate text-sm font-semibold text-slate-100">{p.name}</span>
                        <span className="shrink-0 font-mono text-[10.5px] text-slate-600">{p.slug}</span>
                        {p.archived && <Badge className="bg-white/5 text-slate-400 ring-white/10">archived</Badge>}
                      </div>
                      <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-slate-500">
                        <span>{p.session_count} chat{p.session_count === 1 ? "" : "s"}</span>
                        {p.last_activity > 0 && <span>· last {relativeTime(p.last_activity)}</span>}
                        {p.folders.map((f) => (
                          <span key={f.path} className="truncate rounded bg-white/[0.04] px-1.5 py-0.5 font-mono text-[10.5px]" title={f.path}>
                            {f.label ?? basename(f.path)}
                            {f.primary ? " *" : ""}
                          </span>
                        ))}
                      </div>
                    </div>
                  </button>
                  <div className="flex shrink-0 items-center gap-1">
                    <Button size="sm" variant="ghost" onClick={() => void rename(p)} title="Rename" icon={<Pencil className="size-3.5" />} />
                    <Button
                      size="sm"
                      variant="ghost"
                      title="Add a folder"
                      icon={<FolderPlus className="size-3.5" />}
                      onClick={async () => {
                        const dir = await openDialog({ directory: true, title: "Add a folder to this project" });
                        if (typeof dir === "string") await act(`f:${p.slug}`, () => api.addProjectFolder(p.slug, dir));
                      }}
                    />
                    <Button
                      size="sm"
                      variant="ghost"
                      title={p.archived ? "Restore" : "Archive"}
                      busy={busy === `a:${p.slug}`}
                      icon={p.archived ? <ArchiveRestore className="size-3.5" /> : <Archive className="size-3.5" />}
                      onClick={() => void act(`a:${p.slug}`, () => api.archiveProject(p.slug, !p.archived))}
                    />
                    <Button
                      size="sm"
                      onClick={() => onNewSession(p.primary_path ?? p.folders[0]?.path ?? "", p.slug, null)}
                      icon={<MessageSquarePlus className="size-3.5" />}
                    >
                      New chat
                    </Button>
                  </div>
                </div>
                {expanded && (
                  <div className="border-t border-line px-4 py-2">
                    {busy === `s:${p.slug}` && <Loader2 className="my-2 size-4 animate-spin text-slate-500" />}
                    {list.length === 0 && busy !== `s:${p.slug}` && <div className="py-2 text-[11.5px] text-slate-600">No sessions in these folders yet.</div>}
                    <div className="divide-y divide-line/50">
                      {list.map((s) => (
                        <div key={s.id} className="flex items-center gap-2 py-1.5">
                          {s.parent_session_id && <GitBranch className="ml-4 size-3 shrink-0 text-sky-400/70" />}
                          <button onClick={() => onOpenSession(s)} className="min-w-0 flex-1 text-left">
                            <div className="truncate text-[12.5px] text-slate-200">{s.title}</div>
                            <div className="text-[10.5px] text-slate-500">
                              {relativeTime(s.last_activity)} · {s.message_count} msgs
                              {s.branch_title ? ` · branch: ${s.branch_title}` : ""}
                            </div>
                          </button>
                          <Button
                            size="sm"
                            variant="ghost"
                            title="Start a separate chat from this one"
                            onClick={() => onNewSession(s.cwd || p.primary_path || "", p.slug, s.id)}
                            icon={<GitBranch className="size-3.5" />}
                          />
                        </div>
                      ))}
                    </div>
                  </div>
                )}
              </Card>
            );
          })}
        </div>
      </div>

      {creating && (
        <Modal
          title="New project"
          onClose={() => setCreating(false)}
          footer={
            <>
              <Button variant="ghost" onClick={() => setCreating(false)}>
                Cancel
              </Button>
              <Button variant="primary" busy={busy === "create"} disabled={!name.trim() || folders.length === 0} onClick={() => void create()} icon={<Plus className="size-4" />}>
                Create in Hermes
              </Button>
            </>
          }
        >
          <div className="space-y-3">
            <Field label="Name" hint="Hermes derives a slug from this. The display name can change later.">
              <input autoFocus className={inputClass} value={name} onChange={(e) => setName(e.target.value)} placeholder="Payments platform" />
            </Field>
            <Field label="Folders" hint="The first folder becomes the primary, which Hermes uses for worktrees.">
              <div className="space-y-1.5">
                {folders.map((f) => (
                  <div key={f} className="flex items-center gap-2 rounded-lg bg-canvas px-2.5 py-1.5 font-mono text-[11px] text-slate-300">
                    <span className="min-w-0 flex-1 truncate">{f}</span>
                    <button onClick={() => setFolders(folders.filter((x) => x !== f))} className="text-slate-500 hover:text-red-300">
                      remove
                    </button>
                  </div>
                ))}
                <Button onClick={() => void pickFolder()} icon={<FolderPlus className="size-4" />}>
                  Add folder
                </Button>
              </div>
            </Field>
          </div>
        </Modal>
      )}
    </div>
  );
}
