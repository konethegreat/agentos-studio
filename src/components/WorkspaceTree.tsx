import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  Archive,
  ArchiveRestore,
  BookDown,
  ChevronDown,
  ChevronRight,
  FileText,
  Folder as FolderIcon,
  FolderGit2,
  FolderOpen,
  FolderPlus,
  Link2,
  Loader2,
  MessageSquarePlus,
  MoreHorizontal,
  Paperclip,
  Pencil,
  Pin,
  PinOff,
  Plus,
  Search,
  Tag,
  Trash2,
  X,
} from "lucide-react";
import { Fragment, useEffect, useMemo, useRef, useState, type DragEvent, type KeyboardEvent, type ReactNode } from "react";
import { errorText } from "../lib/api";
import { relativeTime } from "../lib/chat";
import { basename } from "../lib/format";
import { studio, type Attachment, type Folder, type OrgSnapshot, type PlacedChat, type Workspace } from "../lib/studio";
import { ApprovalDeclined, useGuarded } from "./ApprovalProvider";
import { useToast } from "./Toasts";

type DragItem = { kind: "chat" | "folder"; id: string };
const MIME = "application/x-agentos-item";

function useCollapsed() {
  const [set, setSet] = useState<Set<string>>(() => new Set(JSON.parse(localStorage.getItem("tree-collapsed") ?? "[]") as string[]));
  const toggle = (id: string) =>
    setSet((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      localStorage.setItem("tree-collapsed", JSON.stringify([...n]));
      return n;
    });
  return { has: (id: string) => set.has(id), toggle };
}

function Menu({ items, label }: { items: { label: string; icon: ReactNode; onClick: () => void; danger?: boolean; hint?: string }[]; label: string }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  return (
    <div ref={ref} className="relative">
      <button
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
        className="rounded p-0.5 text-slate-500 hover:bg-white/10 hover:text-slate-200"
        title={label}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
      >
        <MoreHorizontal className="size-3.5" />
      </button>
      {open && (
        <div role="menu" className="absolute right-0 top-6 z-40 w-56 rounded-lg border border-line bg-card p-1 shadow-2xl" onKeyDown={(e) => e.key === "Escape" && setOpen(false)}>
          {items.map((it) => (
            <button
              key={it.label}
              role="menuitem"
              title={it.hint}
              onClick={(e) => {
                e.stopPropagation();
                setOpen(false);
                it.onClick();
              }}
              className={`flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-xs ${it.danger ? "text-red-300 hover:bg-red-500/10" : "text-slate-200 hover:bg-white/5"}`}
            >
              {it.icon}
              {it.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function InlineEdit({ value, onSave, onCancel, placeholder }: { value: string; onSave: (v: string) => void; onCancel: () => void; placeholder?: string }) {
  const [v, setV] = useState(value);
  return (
    <input
      autoFocus
      value={v}
      placeholder={placeholder}
      onChange={(e) => setV(e.target.value)}
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        if (e.key === "Enter") onSave(v);
        if (e.key === "Escape") onCancel();
      }}
      onBlur={() => onSave(v)}
      className="w-full min-w-0 rounded-md bg-canvas px-1.5 py-0.5 text-xs text-slate-100 outline-none ring-1 ring-emerald-500/50"
    />
  );
}

function TagChips({ tags }: { tags: string[] }) {
  if (!tags.length) return null;
  return (
    <span className="flex flex-wrap gap-0.5">
      {tags.map((t) => (
        <span key={t} className="rounded bg-sky-500/10 px-1 text-micro text-sky-300">
          #{t}
        </span>
      ))}
    </span>
  );
}

export interface TreeActions {
  onOpen: (c: PlacedChat) => void;
  onRename: (id: string, title: string) => Promise<void>;
  onExport: (id: string) => void;
  onDelete: (id: string) => Promise<void>;
  onNewChatIn: (f: Folder) => void;
}

export function WorkspaceTree({
  snapshot,
  loading,
  query,
  setQuery,
  current,
  reload,
  actions,
}: {
  snapshot: OrgSnapshot | null;
  loading: boolean;
  query: string;
  setQuery: (q: string) => void;
  current: string | null;
  reload: () => void;
  actions: TreeActions;
}) {
  const toast = useToast();
  const guarded = useGuarded();
  const collapsed = useCollapsed();
  const [showArchived, setShowArchived] = useState(false);
  const [editing, setEditing] = useState<{ kind: "chat" | "folder" | "workspace" | "tags-chat" | "tags-folder"; id: string } | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);

  const snap = snapshot ?? { workspaces: [], folders: [], attachments: [], tags: [], projects: [], chats: [] };

  const run = async (p: Promise<unknown>, ok?: string) => {
    try {
      await p;
      if (ok) toast("ok", ok);
      reload();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    }
  };

  // Client-side filter on top of Hermes's search: folder names, tags and directory names.
  const q = query.trim().toLowerCase();
  const tagQuery = q.startsWith("#") ? q.slice(1) : null;
  const chatVisible = (c: PlacedChat) => {
    if (c.archived && !showArchived) return false;
    if (tagQuery !== null) return c.tags.some((t) => t.startsWith(tagQuery));
    return true;
  };
  const folderVisible = (f: Folder) => showArchived || !f.archived;

  const chats = snap.chats.filter(chatVisible);
  const byFolder = useMemo(() => {
    const m = new Map<string, PlacedChat[]>();
    for (const c of chats) {
      const key = c.folder_id ?? (c.workspace_id ? `ws:${c.workspace_id}` : "unfiled");
      m.set(key, [...(m.get(key) ?? []), c]);
    }
    return m;
  }, [chats]);
  const children = useMemo(() => {
    const m = new Map<string, Folder[]>();
    for (const f of snap.folders.filter(folderVisible)) {
      const key = f.parent_id ?? `ws:${f.workspace_id}`;
      m.set(key, [...(m.get(key) ?? []), f]);
    }
    return m;
  }, [snap.folders, showArchived]);
  const attachmentsOf = (kind: "chat" | "folder", id: string) => snap.attachments.filter((a) => a.owner_kind === kind && a.owner_id === id);
  const pinned = chats.filter((c) => c.studio_pinned || c.pinned);

  // ---------------------------------------------------------------- drag and drop
  function onDragStart(e: DragEvent, item: DragItem) {
    e.dataTransfer.setData(MIME, JSON.stringify(item));
    e.dataTransfer.effectAllowed = "move";
  }
  function dropProps(target: string, accept: (it: DragItem) => Promise<unknown> | null) {
    return {
      onDragOver: (e: DragEvent) => {
        if (!e.dataTransfer.types.includes(MIME)) return;
        e.preventDefault();
        e.stopPropagation();
        setDropTarget(target);
      },
      onDragLeave: () => setDropTarget((t) => (t === target ? null : t)),
      onDrop: (e: DragEvent) => {
        e.preventDefault();
        e.stopPropagation();
        setDropTarget(null);
        try {
          const it = JSON.parse(e.dataTransfer.getData(MIME)) as DragItem;
          const p = accept(it);
          if (p) void run(p);
        } catch {
          // not one of ours
        }
      },
    };
  }
  const intoFolder = (f: Folder) => (it: DragItem) =>
    it.kind === "chat" ? studio.org.placeChat(it.id, f.id, null) : it.id === f.id ? null : studio.org.updateFolder(it.id, { parent_id: f.id });
  const intoWorkspace = (w: Workspace) => (it: DragItem) =>
    it.kind === "chat" ? studio.org.placeChat(it.id, null, w.id) : studio.org.updateFolder(it.id, { workspace_id: w.id, parent_id: "" });
  const intoUnfiled = (it: DragItem) => (it.kind === "chat" ? studio.org.placeChat(it.id, null, null) : null);

  // ---------------------------------------------------------------- actions
  async function newWorkspace() {
    try {
      const w = await studio.org.createWorkspace("New workspace");
      reload();
      setEditing({ kind: "workspace", id: w.id });
    } catch (e) {
      toast("error", errorText(e));
    }
  }
  async function newFolder(workspaceId: string, parentId: string | null) {
    try {
      const id = await studio.org.createFolder(workspaceId, parentId, "New folder", null);
      reload();
      setEditing({ kind: "folder", id });
    } catch (e) {
      toast("error", errorText(e));
    }
  }
  async function mapDirectory(f: Folder) {
    const dir = await openDialog({ directory: true, title: `Directory for ${f.name}` });
    if (typeof dir === "string") void run(studio.org.updateFolder(f.id, { path: dir }), `${f.name} now maps to ${dir}`);
  }
  async function attach(kind: "chat" | "folder", id: string) {
    const files = await openDialog({ multiple: true, title: "Attach files" });
    const list = Array.isArray(files) ? files : typeof files === "string" ? [files] : [];
    if (!list.length) return;
    void run(Promise.all(list.map((p) => studio.org.addAttachment(kind, id, p))), `${list.length} file${list.length === 1 ? "" : "s"} attached`);
  }
  function saveTags(kind: "chat" | "folder", id: string, raw: string) {
    setEditing(null);
    void run(studio.org.setTags(kind, id, raw.split(/[,\s]+/).filter(Boolean)));
  }

  // ---------------------------------------------------------------- rows
  // Render helpers are plain functions (not components) so inline editors keep focus across re-renders.
  function attachmentRows(list: Attachment[], depth: number) {
    return (
      <Fragment key={`att-${depth}`}>
        {list.map((a) => (
          <div key={a.id} className="group flex items-center gap-1.5 py-0.5 pr-2 text-label text-slate-500" style={{ paddingLeft: depth * 12 + 22 }} title={a.path}>
            <FileText className={`size-3 shrink-0 ${a.exists ? "" : "text-danger"}`} />
            <button onClick={() => void studio.os.reveal(a.path).catch((e) => toast("error", errorText(e)))} className="min-w-0 flex-1 truncate text-left hover:text-slate-300">
              {a.label ?? basename(a.path)}
              {!a.exists && " (missing)"}
            </button>
            <button onClick={() => void run(studio.org.removeAttachment(a.id))} className="hidden rounded p-0.5 hover:bg-white/10 hover:text-slate-200 group-hover:block" title="Remove attachment (the file is not deleted)" aria-label="Remove attachment">
              <X className="size-3" />
            </button>
          </div>
        ))}
      </Fragment>
    );
  }

  function chatRow(c: PlacedChat, depth: number, keyPrefix = "") {
    const isPinned = c.studio_pinned || c.pinned;
    const att = attachmentsOf("chat", c.id);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "F2") setEditing({ kind: "chat", id: c.id });
      if (e.key === "Delete") void actions.onDelete(c.id);
    };
    return (
      <div key={`${keyPrefix}${c.id}`}>
        <div
          draggable
          onDragStart={(e) => onDragStart(e, { kind: "chat", id: c.id })}
          className={`group flex items-center gap-1 rounded-md py-1 pr-1.5 ${current === c.id ? "bg-white/[0.08]" : "hover:bg-white/[0.03]"} ${c.archived ? "opacity-50" : ""}`}
          style={{ paddingLeft: depth * 12 + 8 }}
        >
          {editing?.kind === "chat" && editing.id === c.id ? (
            <InlineEdit
              value={c.title}
              onCancel={() => setEditing(null)}
              onSave={(v) => {
                setEditing(null);
                if (v.trim() && v !== c.title) void actions.onRename(c.id, v.trim()).then(reload);
              }}
            />
          ) : editing?.kind === "tags-chat" && editing.id === c.id ? (
            <InlineEdit value={c.tags.join(", ")} placeholder="bug, ui, release" onCancel={() => setEditing(null)} onSave={(v) => saveTags("chat", c.id, v)} />
          ) : (
            <button onClick={() => actions.onOpen(c)} onKeyDown={onKey} className="min-w-0 flex-1 text-left" title={`${c.title}\n${c.cwd}\nF2 rename, Delete removes`}>
              <div className="flex items-center gap-1">
                {isPinned && <Pin className="size-2.5 shrink-0 text-amber-300" />}
                <span className="truncate text-ui text-slate-200">{c.title}</span>
              </div>
              <div className="flex items-center gap-1.5 text-2xs text-slate-500">
                <span>{relativeTime(c.last_activity)}</span>
                <span>{c.message_count} msgs</span>
                {att.length > 0 && (
                  <span className="inline-flex items-center gap-0.5">
                    <Paperclip className="size-2.5" />
                    {att.length}
                  </span>
                )}
                {c.auto && <span title="Placed here because it ran inside this folder's directory">auto</span>}
                <TagChips tags={c.tags} />
              </div>
            </button>
          )}
          <div className="hidden items-center group-focus-within:flex group-hover:flex">
            <button
              onClick={() => void run(studio.org.setChatFlags(c.id, { pinned: !c.studio_pinned }))}
              className="rounded p-0.5 text-slate-500 hover:bg-white/10 hover:text-amber-300"
              title={c.studio_pinned ? "Unpin" : "Pin to the top"}
              aria-label={c.studio_pinned ? "Unpin" : "Pin"}
            >
              {c.studio_pinned ? <PinOff className="size-3" /> : <Pin className="size-3" />}
            </button>
            <Menu
              label="Chat actions"
              items={[
                { label: "Rename", icon: <Pencil className="size-3.5" />, onClick: () => setEditing({ kind: "chat", id: c.id }), hint: "F2" },
                { label: "Edit tags", icon: <Tag className="size-3.5" />, onClick: () => setEditing({ kind: "tags-chat", id: c.id }) },
                { label: "Attach files", icon: <Paperclip className="size-3.5" />, onClick: () => void attach("chat", c.id), hint: "Attached files are listed at the top of every message you send in this chat" },
                { label: "Export to Obsidian", icon: <BookDown className="size-3.5" />, onClick: () => actions.onExport(c.id) },
                { label: "Back to automatic placement", icon: <FolderOpen className="size-3.5" />, onClick: () => void run(studio.org.placeChat(c.id, null, null)) },
                c.archived
                  ? { label: "Unarchive", icon: <ArchiveRestore className="size-3.5" />, onClick: () => void run(studio.org.setChatFlags(c.id, { archived: false })) }
                  : { label: "Archive", icon: <Archive className="size-3.5" />, onClick: () => void run(studio.org.setChatFlags(c.id, { archived: true })), hint: "Hidden until you show archived items" },
                { label: "Delete from Hermes", icon: <Trash2 className="size-3.5" />, onClick: () => void actions.onDelete(c.id), danger: true },
              ]}
            />
          </div>
        </div>
        {attachmentRows(att, depth)}
      </div>
    );
  }

  function folderNode(f: Folder, depth: number): ReactNode {
    const open = !collapsed.has(f.id);
    const kids = children.get(f.id) ?? [];
    const fchats = byFolder.get(f.id) ?? [];
    const att = attachmentsOf("folder", f.id);
    const count = fchats.length;
    return (
      <div key={f.id}>
        <div
          draggable
          onDragStart={(e) => onDragStart(e, { kind: "folder", id: f.id })}
          {...dropProps(f.id, intoFolder(f))}
          className={`group flex items-center gap-1 rounded-md py-1 pr-1.5 ${dropTarget === f.id ? "bg-emerald-500/15 ring-1 ring-emerald-500/40" : "hover:bg-white/[0.03]"} ${f.archived ? "opacity-50" : ""}`}
          style={{ paddingLeft: depth * 12 + 2 }}
        >
          <button onClick={() => collapsed.toggle(f.id)} className="rounded p-0.5 text-slate-500 hover:text-slate-200" aria-label={open ? "Collapse" : "Expand"} aria-expanded={open}>
            {open ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
          </button>
          {f.path ? <FolderGit2 className="size-3.5 shrink-0 text-ok/80" /> : <FolderIcon className="size-3.5 shrink-0 text-slate-400" />}
          {editing?.kind === "folder" && editing.id === f.id ? (
            <InlineEdit
              value={f.name}
              onCancel={() => setEditing(null)}
              onSave={(v) => {
                setEditing(null);
                if (v.trim() && v !== f.name) void run(studio.org.updateFolder(f.id, { name: v.trim() }));
              }}
            />
          ) : editing?.kind === "tags-folder" && editing.id === f.id ? (
            <InlineEdit value={f.tags.join(", ")} placeholder="client, rust" onCancel={() => setEditing(null)} onSave={(v) => saveTags("folder", f.id, v)} />
          ) : (
            <button
              onClick={() => collapsed.toggle(f.id)}
              onKeyDown={(e) => e.key === "F2" && setEditing({ kind: "folder", id: f.id })}
              className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
              title={`${f.name}${f.path ? `\n${f.path}` : "\nNot mapped to a directory"}${f.project_slug ? `\nHermes project: ${f.project_slug}` : ""}`}
            >
              {f.pinned && <Pin className="size-2.5 shrink-0 text-amber-300" />}
              <span className="truncate text-ui font-medium text-slate-300">{f.name}</span>
              {count > 0 && <span className="text-2xs text-slate-600">{count}</span>}
              {f.project_slug && <span className="rounded bg-violet-500/10 px-1 text-micro text-violet-300">{f.project_slug}</span>}
              <TagChips tags={f.tags} />
            </button>
          )}
          <div className="hidden items-center group-focus-within:flex group-hover:flex">
            <button onClick={() => actions.onNewChatIn(f)} className="rounded p-0.5 text-slate-500 hover:bg-white/10 hover:text-emerald-300" title="New chat in this folder" aria-label="New chat in this folder">
              <MessageSquarePlus className="size-3" />
            </button>
            <Menu
              label="Folder actions"
              items={[
                { label: "New sub-folder", icon: <FolderPlus className="size-3.5" />, onClick: () => void newFolder(f.workspace_id, f.id) },
                { label: "Rename", icon: <Pencil className="size-3.5" />, onClick: () => setEditing({ kind: "folder", id: f.id }), hint: "F2" },
                { label: f.path ? "Change directory" : "Map to a directory", icon: <FolderOpen className="size-3.5" />, onClick: () => void mapDirectory(f), hint: "Chats run inside this directory appear here automatically" },
                ...(f.path ? [{ label: "Clear directory", icon: <X className="size-3.5" />, onClick: () => void run(studio.org.updateFolder(f.id, { path: "" })) }] : []),
                ...(f.path
                  ? [
                      {
                        label: f.project_slug ? `Hermes project: ${f.project_slug}` : "Link to a Hermes project",
                        icon: <Link2 className="size-3.5" />,
                        onClick: () => void run(guarded((a) => studio.org.linkProject(f.id, a)), "Linked to a Hermes project"),
                        hint: "Reuses the Hermes project that contains this directory, or runs hermes project create",
                      },
                    ]
                  : []),
                { label: "Attach files", icon: <Paperclip className="size-3.5" />, onClick: () => void attach("folder", f.id), hint: "Listed for every chat in this folder and its sub-folders" },
                { label: "Edit tags", icon: <Tag className="size-3.5" />, onClick: () => setEditing({ kind: "tags-folder", id: f.id }) },
                { label: f.pinned ? "Unpin" : "Pin to the top", icon: f.pinned ? <PinOff className="size-3.5" /> : <Pin className="size-3.5" />, onClick: () => void run(studio.org.updateFolder(f.id, { pinned: !f.pinned })) },
                f.archived
                  ? { label: "Unarchive", icon: <ArchiveRestore className="size-3.5" />, onClick: () => void run(studio.org.updateFolder(f.id, { archived: false })) }
                  : { label: "Archive", icon: <Archive className="size-3.5" />, onClick: () => void run(studio.org.updateFolder(f.id, { archived: true })) },
                { label: "Delete folder", icon: <Trash2 className="size-3.5" />, danger: true, onClick: () => void run(guarded((a) => studio.org.deleteFolder(f.id, a)), "Folder deleted. Its chats were kept.") },
              ]}
            />
          </div>
        </div>
        {open && (
          <div>
            {kids.map((k) => (
              folderNode(k, depth + 1)
            ))}
            {fchats.map((c) => (
              chatRow(c, depth + 1)
            ))}
            {attachmentRows(att, depth)}
          </div>
        )}
      </div>
    );
  }

  const unfiled = byFolder.get("unfiled") ?? [];
  const workspaces = snap.workspaces.filter((w) => showArchived || !w.archived);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="space-y-1.5 px-3 pb-2">
        <div className="flex items-center gap-2 rounded-lg bg-card px-2 ring-1 ring-line">
          <Search className="size-3.5 text-slate-500" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search chats, or #tag"
            aria-label="Search chats"
            className="h-8 min-w-0 flex-1 bg-transparent text-xs text-slate-100 outline-none placeholder:text-slate-600"
          />
          {loading && <Loader2 className="size-3.5 animate-spin text-slate-500" />}
        </div>
        <div className="flex items-center gap-2 text-label text-slate-500">
          <button onClick={() => void newWorkspace()} className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-white/5 hover:text-slate-200" title="A workspace groups folders, for example Work or a client">
            <Plus className="size-3" /> Workspace
          </button>
          <label className="ml-auto inline-flex cursor-pointer items-center gap-1" title="Show archived workspaces, folders and chats">
            <input type="checkbox" checked={showArchived} onChange={(e) => setShowArchived(e.target.checked)} className="accent-emerald-500" />
            Archived
          </label>
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3" role="tree" aria-label="Workspaces and chats">
        {pinned.length > 0 && (
          <Section title="Pinned" id="pinned" collapsed={collapsed}>
            {pinned.map((c) => (
              chatRow(c, 0, "pin-")
            ))}
          </Section>
        )}
        {workspaces.map((w) => (
          <div key={w.id} className="mt-1">
            <div
              {...dropProps(`ws:${w.id}`, intoWorkspace(w))}
              className={`group flex items-center gap-1 rounded-md px-1 py-1 ${dropTarget === `ws:${w.id}` ? "bg-emerald-500/15 ring-1 ring-emerald-500/40" : ""} ${w.archived ? "opacity-50" : ""}`}
            >
              <button onClick={() => collapsed.toggle(w.id)} className="rounded p-0.5 text-slate-500 hover:text-slate-200" aria-label={collapsed.has(w.id) ? "Expand" : "Collapse"}>
                {collapsed.has(w.id) ? <ChevronRight className="size-3" /> : <ChevronDown className="size-3" />}
              </button>
              {editing?.kind === "workspace" && editing.id === w.id ? (
                <InlineEdit
                  value={w.name}
                  onCancel={() => setEditing(null)}
                  onSave={(v) => {
                    setEditing(null);
                    if (v.trim() && v !== w.name) void run(studio.org.updateWorkspace(w.id, { name: v.trim() }));
                  }}
                />
              ) : (
                <button
                  onClick={() => collapsed.toggle(w.id)}
                  onKeyDown={(e) => e.key === "F2" && setEditing({ kind: "workspace", id: w.id })}
                  className="min-w-0 flex-1 truncate text-left text-caption font-semibold uppercase tracking-wider text-slate-400"
                  title="Drop chats or folders here"
                >
                  {w.name}
                </button>
              )}
              <div className="hidden items-center group-focus-within:flex group-hover:flex">
                <button onClick={() => void newFolder(w.id, null)} className="rounded p-0.5 text-slate-500 hover:bg-white/10 hover:text-slate-200" title="New folder" aria-label="New folder">
                  <FolderPlus className="size-3" />
                </button>
                <Menu
                  label="Workspace actions"
                  items={[
                    { label: "Rename", icon: <Pencil className="size-3.5" />, onClick: () => setEditing({ kind: "workspace", id: w.id }) },
                    w.archived
                      ? { label: "Unarchive", icon: <ArchiveRestore className="size-3.5" />, onClick: () => void run(studio.org.updateWorkspace(w.id, { archived: false })) }
                      : { label: "Archive", icon: <Archive className="size-3.5" />, onClick: () => void run(studio.org.updateWorkspace(w.id, { archived: true })) },
                    { label: "Delete workspace", icon: <Trash2 className="size-3.5" />, danger: true, onClick: () => void run(guarded((a) => studio.org.deleteWorkspace(w.id, a)), "Workspace deleted. Its chats were kept.") },
                  ]}
                />
              </div>
            </div>
            {!collapsed.has(w.id) && (
              <div>
                {(children.get(`ws:${w.id}`) ?? []).map((f) => (
                  folderNode(f, 0)
                ))}
                {(byFolder.get(`ws:${w.id}`) ?? []).map((c) => (
                  chatRow(c, 0)
                ))}
                {(children.get(`ws:${w.id}`) ?? []).length === 0 && (byFolder.get(`ws:${w.id}`) ?? []).length === 0 && (
                  <div className="px-6 py-1.5 text-label text-slate-600">Empty. Add a folder or drag chats here.</div>
                )}
              </div>
            )}
          </div>
        ))}
        <div className="mt-1" {...dropProps("unfiled", intoUnfiled)}>
          <Section title={workspaces.length ? "Unfiled" : "Chats"} id="unfiled" collapsed={collapsed} highlight={dropTarget === "unfiled"}>
            {unfiled.length === 0 && !loading && <div className="px-3 py-3 text-center text-label text-slate-600">{query ? "No chats match" : workspaces.length ? "Everything is filed" : "No Hermes chats yet"}</div>}
            {unfiled.map((c) => (
              chatRow(c, 0)
            ))}
          </Section>
        </div>
        {workspaces.length === 0 && (
          <div className="mx-2 mt-3 rounded-lg border border-dashed border-line px-3 py-3 text-label leading-relaxed text-slate-500">
            Group chats into workspaces and folders. Map a folder to a repository and every chat run there lands in it automatically.
            <button onClick={() => void newWorkspace()} className="mt-2 block text-emerald-300 hover:underline">
              Create your first workspace
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

function Section({ title, id, collapsed, children, highlight }: { title: string; id: string; collapsed: ReturnType<typeof useCollapsed>; children: ReactNode; highlight?: boolean }) {
  const closed = collapsed.has(`section:${id}`);
  return (
    <div className={`rounded-md ${highlight ? "bg-emerald-500/10 ring-1 ring-emerald-500/40" : ""}`}>
      <button onClick={() => collapsed.toggle(`section:${id}`)} className="flex w-full items-center gap-1 px-1 py-1 text-left text-caption font-semibold uppercase tracking-wider text-slate-500 hover:text-slate-300" aria-expanded={!closed}>
        {closed ? <ChevronRight className="size-3" /> : <ChevronDown className="size-3" />}
        {title}
      </button>
      {!closed && children}
    </div>
  );
}
