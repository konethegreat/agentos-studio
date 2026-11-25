import { useCallback, useEffect, useMemo, useState } from "react";
import {
  AppWindow,
  CheckCircle2,
  CircleAlert,
  Copy,
  ExternalLink,
  Eye,
  FilePlus2,
  Globe,
  Package,
  Plus,
  RefreshCw,
  Save,
  Trash2,
  UploadCloud,
} from "lucide-react";
import { errorText } from "../lib/api";
import { ApprovalDeclined, useGuarded } from "../components/ApprovalProvider";
import { useToast } from "../components/Toasts";
import { Badge, Button, Empty, Field, Modal, inputClass } from "../components/ui";
import { studio, type Artifact, type ArtifactFile, type ArtifactSite, type ArtifactTemplate } from "../lib/studio";

const TEMPLATE_ORDER = ["blank", "todo", "notes"];

function bytesLabel(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function dateLabel(ts: number) {
  return new Date(ts * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

function fileLanguage(path: string) {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "html") return "HTML";
  if (ext === "css") return "CSS";
  if (ext === "js" || ext === "mjs") return "JavaScript";
  if (ext === "json" || ext === "webmanifest") return "JSON";
  if (ext === "svg") return "SVG";
  return "Text";
}

function escapeHtml(s: string) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function starterFor(path: string, artifact: Artifact) {
  const name = escapeHtml(artifact.name);
  if (path.endsWith(".css")) return "/* Styles for this page. */\n";
  if (path.endsWith(".js") || path.endsWith(".mjs")) return "// Script for this page. AgentOS is available from agentos.js.\n";
  if (path.endsWith(".json") || path.endsWith(".webmanifest")) return "{}\n";
  if (path.endsWith(".md")) return `# ${artifact.name}\n`;
  if (!path.endsWith(".html")) return "";
  return `<!doctype html>
<html lang="en" data-artifact="${artifact.slug}">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>${name}</title>
</head>
<body>
  <h1>${name}</h1>
  <p>New page. Link styles.css and agentos.js if you want the shared helpers.</p>
</body>
</html>
`;
}

/**
 * Code > Artifacts: small web apps that preview over a token-guarded loopback server, install as
 * their own window, and publish to one GitHub Pages site with a generated gallery.
 */
export function Artifacts() {
  const toast = useToast();
  const guarded = useGuarded();
  const [rows, setRows] = useState<Artifact[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [files, setFiles] = useState<ArtifactFile[]>([]);
  const [file, setFile] = useState("");
  const [content, setContent] = useState("");
  const [saved, setSaved] = useState("");
  const [loadingFiles, setLoadingFiles] = useState(false);
  const [loadingFile, setLoadingFile] = useState(false);
  const [saving, setSaving] = useState(false);
  const [publishing, setPublishing] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [installed, setInstalled] = useState<string[]>([]);
  const [templates, setTemplates] = useState<ArtifactTemplate[]>([]);
  const [site, setSite] = useState<ArtifactSite | null>(null);
  const [siteError, setSiteError] = useState<string | null>(null);

  const [createOpen, setCreateOpen] = useState(false);
  const [createName, setCreateName] = useState("");
  const [createDescription, setCreateDescription] = useState("");
  const [createTemplate, setCreateTemplate] = useState("blank");
  const [creating, setCreating] = useState(false);

  const [adding, setAdding] = useState(false);
  const [newPath, setNewPath] = useState("");

  const [siteOpen, setSiteOpen] = useState(false);
  const [repoDraft, setRepoDraft] = useState("");
  const [domainDraft, setDomainDraft] = useState("");
  const [savingSite, setSavingSite] = useState(false);
  const [savingDomain, setSavingDomain] = useState(false);

  const [confirmDelete, setConfirmDelete] = useState(false);
  const [lastPublished, setLastPublished] = useState<string | null>(null);

  const artifact = useMemo(() => rows?.find((r) => r.id === selected) ?? null, [rows, selected]);
  const generated = file === "agentos.js";
  const dirty = selected !== null && file !== "" && content !== saved && !generated;

  const load = useCallback(async () => {
    try {
      const list = await studio.artifacts.list();
      setRows(list);
      setSelected((current) => (current && list.some((a) => a.id === current) ? current : list[0]?.id ?? null));
    } catch (e) {
      toast("error", errorText(e));
      setRows([]);
    }
    try {
      setTemplates(await studio.artifacts.templates());
    } catch (e) {
      toast("error", errorText(e));
    }
    try {
      setInstalled(await studio.artifacts.installed());
    } catch {
      setInstalled([]);
    }
    try {
      const s = await studio.artifacts.site();
      setSite(s);
      setSiteError(s.error);
    } catch (e) {
      setSite(null);
      setSiteError(errorText(e));
    }
  }, [toast]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!selected) {
      setFiles([]);
      setFile("");
      setContent("");
      setSaved("");
      return;
    }
    let live = true;
    setLoadingFiles(true);
    setFiles([]);
    setFile("");
    setContent("");
    setSaved("");
    studio.artifacts
      .files(selected)
      .then((list) => {
        if (!live) return;
        setFiles(list);
        setFile((current) => {
          if (current && list.some((f) => f.path === current)) return current;
          return list.find((f) => f.path === "index.html")?.path ?? list[0]?.path ?? "";
        });
      })
      .catch((e) => {
        if (live) toast("error", errorText(e));
      })
      .finally(() => {
        if (live) setLoadingFiles(false);
      });
    return () => {
      live = false;
    };
  }, [selected, toast]);

  useEffect(() => {
    if (!selected || !file) return;
    let live = true;
    setLoadingFile(true);
    studio.artifacts
      .readFile(selected, file)
      .then((text) => {
        if (!live) return;
        setContent(text);
        setSaved(text);
      })
      .catch((e) => {
        if (live) toast("error", errorText(e));
      })
      .finally(() => {
        if (live) setLoadingFile(false);
      });
    return () => {
      live = false;
    };
  }, [selected, file, toast]);

  const save = useCallback(async (): Promise<boolean> => {
    if (!selected || !file || loadingFile || file === "agentos.js") return false;
    if (content === saved) return true;
    setSaving(true);
    try {
      const row = await studio.artifacts.writeFile(selected, file, content);
      setRows((list) => list?.map((r) => (r.id === row.id ? row : r)) ?? list);
      setSaved(content);
      setFiles(await studio.artifacts.files(selected));
      toast("ok", `Saved ${file}`);
      return true;
    } catch (e) {
      toast("error", errorText(e));
      return false;
    } finally {
      setSaving(false);
    }
  }, [selected, file, content, saved, loadingFile, toast]);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void save();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [save]);

  async function choose(id: string) {
    if (id === selected) return;
    if (dirty && !(await save())) return;
    setSelected(id);
    setLastPublished(null);
  }

  async function pickFile(path: string) {
    if (path === file) return;
    if (dirty && !(await save())) return;
    setFile(path);
  }

  async function create() {
    const name = createName.trim();
    if (!name) {
      toast("error", "Give the artifact a name");
      return;
    }
    setCreating(true);
    try {
      const a = await studio.artifacts.create(name, createDescription.trim(), createTemplate);
      setRows((list) => [a, ...(list ?? [])]);
      setSelected(a.id);
      setCreateOpen(false);
      setCreateName("");
      setCreateDescription("");
      setCreateTemplate("blank");
      toast("ok", `${a.name} created at ${a.slug}`);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setCreating(false);
    }
  }

  async function addFile() {
    const path = newPath.trim();
    if (!selected || !artifact || !path) return;
    if (files.some((f) => f.path === path)) {
      toast("info", `${path} is already open in the tabs`);
      setAdding(false);
      setNewPath("");
      return;
    }
    if (dirty && !(await save())) return;
    try {
      const row = await studio.artifacts.writeFile(selected, path, starterFor(path, artifact));
      setRows((list) => list?.map((r) => (r.id === row.id ? row : r)) ?? list);
      setFiles(await studio.artifacts.files(selected));
      setFile(path);
      setNewPath("");
      setAdding(false);
      toast("ok", `${path} added`);
    } catch (e) {
      toast("error", errorText(e));
    }
  }

  async function publish() {
    if (!selected || !artifact) return;
    setPublishing(true);
    try {
      if (dirty && !(await save())) return;
      const result = await guarded((a) => studio.artifacts.publish(selected, a));
      setRows((list) => list?.map((r) => (r.id === result.artifact.id ? result.artifact : r)) ?? list);
      setLastPublished(result.url);
      toast("ok", `Published to ${result.site_repo}. The site rebuilds in about a minute.`);
      try {
        const s = await studio.artifacts.site();
        setSite(s);
        setSiteError(s.error);
      } catch (e) {
        setSiteError(errorText(e));
      }
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setPublishing(false);
    }
  }

  async function toggleInstall(a: Artifact) {
    try {
      if (installed.includes(a.id)) {
        await studio.artifacts.uninstall(a.id);
        setInstalled((ids) => ids.filter((id) => id !== a.id));
        toast("ok", `${a.name} will no longer open when Studio starts`);
        return;
      }
      if (!a.live_url) {
        toast("info", "Publish the artifact first. Install opens its public URL.");
        return;
      }
      await studio.artifacts.install(a.id);
      setInstalled((ids) => [...ids, a.id]);
      toast("ok", `${a.name} installed. It opens in its own window on every Studio start.`);
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    }
  }

  async function remove() {
    if (!selected) return;
    setDeleting(true);
    try {
      await guarded((a) => studio.artifacts.remove(selected, a));
      const gone = rows?.find((r) => r.id === selected);
      const next = rows?.filter((r) => r.id !== selected) ?? [];
      setRows(next);
      setSelected(next[0]?.id ?? null);
      setInstalled((ids) => ids.filter((id) => id !== selected));
      setConfirmDelete(false);
      toast("ok", `${gone?.name ?? "Artifact"} deleted from this machine`);
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setDeleting(false);
    }
  }

  function openSite() {
    setRepoDraft(site?.repo ?? "");
    setDomainDraft(site?.cname ?? "");
    setSiteOpen(true);
  }

  async function saveSite() {
    setSavingSite(true);
    try {
      const s = await studio.artifacts.setSite(repoDraft.trim());
      setSite(s);
      setSiteError(s.error);
      setRepoDraft(s.repo);
      toast("ok", `Site repository set to ${s.repo}`);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setSavingSite(false);
    }
  }

  async function saveDomain() {
    setSavingDomain(true);
    try {
      const s = await guarded((a) => studio.artifacts.setDomain(domainDraft.trim(), a));
      setSite(s);
      setSiteError(s.error);
      setDomainDraft(s.cname ?? "");
      toast("ok", s.cname ? `Domain set to ${s.cname}` : "Custom domain cleared");
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setSavingDomain(false);
    }
  }

  async function copy(url: string) {
    try {
      await navigator.clipboard.writeText(url);
      toast("ok", "Link copied");
    } catch {
      toast("error", "Could not copy. Select the link and copy it by hand.");
    }
  }

  const siteReady = !!site?.pages_ready;

  return (
    <section className="flex h-full flex-col">
      <header className="flex shrink-0 items-center justify-between gap-3 border-b border-line px-6 py-3">
        <div className="min-w-0">
          <h1 className="text-base font-semibold text-slate-100">Artifacts</h1>
          <p className="truncate text-label text-slate-500">Small web apps you build here, preview over loopback, install as their own window, and publish to GitHub Pages.</p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <Button size="sm" icon={<RefreshCw className="size-3.5" />} onClick={() => void load()} title="Reload artifacts and site status">
            Refresh
          </Button>
          <Button size="sm" icon={<Globe className="size-3.5" />} onClick={openSite} title="Site repository, public URL, custom domain and the last deploy">
            Site
            <span className={`ml-1 size-1.5 rounded-full ${siteReady ? "bg-emerald-400" : "bg-slate-600"}`} aria-hidden="true" />
          </Button>
          <Button size="sm" variant="primary" icon={<Plus className="size-3.5" />} onClick={() => setCreateOpen(true)}>
            New artifact
          </Button>
        </div>
      </header>

      <div className="flex min-h-0 flex-1">
        <aside className="w-64 shrink-0 overflow-y-auto border-r border-line px-2 py-2" aria-label="Artifacts">
          {rows === null && <p className="px-2 py-3 text-xs text-slate-500">Loading...</p>}
          {rows?.length === 0 && (
            <div className="px-2 py-4">
              <Empty icon={<Package className="size-6" />} title="No artifacts yet" hint="Create one from a template and it is ready to run." />
            </div>
          )}
          {rows?.map((a) => (
            <button
              key={a.id}
              type="button"
              onClick={() => void choose(a.id)}
              aria-current={a.id === selected ? "true" : undefined}
              className={`mb-1 block w-full rounded-lg px-3 py-2 text-left transition-colors ${
                a.id === selected ? "bg-white/[0.07] text-slate-100" : "text-slate-300 hover:bg-white/[0.04]"
              }`}
            >
              <div className="flex items-center gap-2">
                <span className={`size-1.5 shrink-0 rounded-full ${a.published_at ? "bg-emerald-400" : "bg-slate-600"}`} aria-hidden="true" />
                <span className="truncate text-sm font-medium">{a.name}</span>
              </div>
              <div className="mt-0.5 flex items-center gap-1.5 truncate pl-3.5 text-label text-slate-500">
                <span className="truncate">{a.slug}</span>
                <span aria-hidden="true">·</span>
                <span className="shrink-0">{a.files} files</span>
                {installed.includes(a.id) && (
                  <span className="shrink-0 text-emerald-400" title="Installed as an app">
                    app
                  </span>
                )}
              </div>
            </button>
          ))}
        </aside>

        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          {!artifact && (
            <div className="flex flex-1 items-center justify-center p-6">
              <div className="max-w-md text-center">
                <div className="mx-auto grid size-12 place-items-center rounded-2xl bg-emerald-500/10 text-emerald-300 ring-1 ring-emerald-500/20">
                  <Package className="size-6" />
                </div>
                <h2 className="mt-4 text-lg font-semibold text-slate-100">Functional artifacts, not screenshots</h2>
                <p className="mt-2 text-sm leading-relaxed text-slate-400">
                  Every artifact is a real folder of HTML, CSS and JavaScript. It keeps data in the browser, previews in a private
                  window, installs as its own app, and gets a public URL on your GitHub Pages site.
                </p>
                <Button className="mt-5" variant="primary" icon={<Plus className="size-4" />} onClick={() => setCreateOpen(true)}>
                  Create the first artifact
                </Button>
              </div>
            </div>
          )}

          {artifact && (
            <>
              <div className="shrink-0 border-b border-line px-6 py-4">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <h2 className="text-lg font-semibold text-slate-100">{artifact.name}</h2>
                      {artifact.published_at ? (
                        <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/25">Live</Badge>
                      ) : (
                        <Badge className="bg-white/5 text-slate-400 ring-white/10">Draft</Badge>
                      )}
                      <Badge className="bg-white/5 text-slate-400 ring-white/10">{artifact.template}</Badge>
                      {installed.includes(artifact.id) && (
                        <Badge className="bg-sky-500/10 text-sky-300 ring-sky-500/25">Installed</Badge>
                      )}
                    </div>
                    <p className="mt-1 max-w-2xl text-xs leading-relaxed text-slate-400">
                      {artifact.description || "No description yet. Edit the files below, then publish."}
                    </p>
                    <p className="mt-1 font-mono text-label text-slate-600">
                      {artifact.slug} · {bytesLabel(artifact.bytes)} · updated {dateLabel(artifact.updated_at)}
                    </p>
                  </div>
                  {artifact.live_url && (
                    <div className="flex min-w-0 items-center gap-1.5 rounded-lg border border-line bg-canvas px-2 py-1">
                      <a
                        href={artifact.live_url}
                        title={artifact.live_url}
                        onClick={(e) => {
                          e.preventDefault();
                          void studio.os.openExternal(artifact.live_url!).catch((err) => toast("error", errorText(err)));
                        }}
                        className="max-w-[260px] truncate font-mono text-label text-emerald-300 hover:underline"
                      >
                        {artifact.live_url.replace(/^https:\/\//, "")}
                      </a>
                      <button
                        type="button"
                        onClick={() => void copy(artifact.live_url!)}
                        className="rounded p-1 text-slate-500 hover:text-slate-200"
                        title="Copy the public link"
                        aria-label="Copy the public link"
                      >
                        <Copy className="size-3.5" />
                      </button>
                      <button
                        type="button"
                        onClick={() => void studio.os.openExternal(artifact.live_url!).catch((err) => toast("error", errorText(err)))}
                        className="rounded p-1 text-slate-500 hover:text-slate-200"
                        title="Open in the default browser"
                        aria-label="Open in the default browser"
                      >
                        <ExternalLink className="size-3.5" />
                      </button>
                    </div>
                  )}
                </div>
              </div>

              <div className="flex shrink-0 flex-wrap items-center gap-1.5 border-b border-line px-6 py-2">
                <Button
                  size="sm"
                  icon={<Eye className="size-3.5" />}
                  onClick={() => void studio.artifacts.preview(artifact.id).catch((e) => toast("error", errorText(e)))}
                  title="Open a private preview window served from this machine"
                >
                  Preview
                </Button>
                <Button
                  size="sm"
                  variant="primary"
                  icon={<UploadCloud className="size-3.5" />}
                  busy={publishing}
                  onClick={() => void publish()}
                  title="Commit every published artifact and deploy the site"
                >
                  Publish
                </Button>
                <Button
                  size="sm"
                  icon={<AppWindow className="size-3.5" />}
                  onClick={() => void toggleInstall(artifact)}
                  title={installed.includes(artifact.id) ? "Stop opening this app when Studio starts" : "Open this app in its own window on every Studio start"}
                >
                  {installed.includes(artifact.id) ? "Uninstall" : "Install as app"}
                </Button>
                <div className="ml-auto flex items-center gap-1.5">
                  <Button size="sm" icon={<Save className="size-3.5" />} busy={saving} disabled={!dirty || loadingFile || generated} onClick={() => void save()}>
                    Save
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    icon={<Trash2 className="size-3.5" />}
                    onClick={() => setConfirmDelete(true)}
                    title="Delete this artifact and its files from this machine"
                  >
                    Delete
                  </Button>
                </div>
              </div>

              {lastPublished && (
                <div className="flex shrink-0 items-center gap-2 border-b border-emerald-500/20 bg-emerald-500/[0.06] px-6 py-2 text-xs text-emerald-200">
                  <CheckCircle2 className="size-3.5 shrink-0" />
                  <span className="min-w-0 truncate">Published. GitHub Pages is rebuilding at {lastPublished}</span>
                  <button type="button" className="ml-auto shrink-0 underline" onClick={() => void copy(lastPublished)}>
                    Copy link
                  </button>
                </div>
              )}

              <div className="flex shrink-0 items-center gap-1 overflow-x-auto border-b border-line px-4 py-1.5">
                {files.map((f) => (
                  <button
                    key={f.path}
                    type="button"
                    onClick={() => void pickFile(f.path)}
                    title={`${f.path} · ${bytesLabel(f.bytes)}`}
                    className={`flex shrink-0 items-center gap-1.5 rounded-md px-2 py-1 font-mono text-label transition-colors ${
                      f.path === file ? "bg-emerald-500/10 text-emerald-200" : "text-slate-400 hover:bg-white/[0.05] hover:text-slate-200"
                    }`}
                  >
                    {f.path}
                    {dirty && f.path === file && <span className="size-1.5 rounded-full bg-amber-400" aria-label="Unsaved changes" />}
                  </button>
                ))}
                {files.length === 0 && !loadingFiles && <span className="px-2 py-1 text-label text-slate-600">No editable files</span>}
                {adding ? (
                  <form
                    className="flex shrink-0 items-center gap-1"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void addFile();
                    }}
                  >
                    <input
                      autoFocus
                      value={newPath}
                      onChange={(e) => setNewPath(e.target.value)}
                      placeholder="pages/about.html"
                      aria-label="New file path"
                      className="h-7 w-44 rounded-md border border-line bg-canvas px-2 font-mono text-label text-slate-200 outline-none focus:border-emerald-500/60"
                    />
                    <Button size="sm" type="submit" variant="primary" disabled={!newPath.trim()}>
                      Add
                    </Button>
                    <Button size="sm" type="button" variant="ghost" onClick={() => { setAdding(false); setNewPath(""); }}>
                      Cancel
                    </Button>
                  </form>
                ) : (
                  <button
                    type="button"
                    onClick={() => setAdding(true)}
                    className="flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-label text-slate-500 hover:bg-white/[0.05] hover:text-slate-200"
                    title="Add a page, stylesheet or script"
                  >
                    <FilePlus2 className="size-3.5" />
                    New file
                  </button>
                )}
              </div>

              <div className="relative min-h-0 flex-1">
                {loadingFile || loadingFiles ? (
                  <p className="px-6 py-4 text-xs text-slate-500">Loading {file || "files"}...</p>
                ) : file ? (
                  <textarea
                    value={content}
                    onChange={(e) => setContent(e.target.value)}
                    spellCheck={false}
                    readOnly={generated}
                    aria-label={`${file} contents`}
                    className="h-full w-full resize-none border-0 bg-transparent px-6 py-4 font-mono text-ui leading-relaxed text-slate-200 outline-none read-only:text-slate-400"
                  />
                ) : (
                  <div className="p-6">
                    <Empty icon={<FilePlus2 className="size-6" />} title="No files" hint="Add an index.html to make this artifact runnable." />
                  </div>
                )}
              </div>

              <footer className="flex shrink-0 items-center gap-2 border-t border-line px-6 py-1.5 text-label text-slate-500">
                <span>{fileLanguage(file)}</span>
                <span aria-hidden="true">·</span>
                <span>{generated ? "agentos.js is generated and refreshed on publish" : dirty ? "Unsaved changes" : "Saved"}</span>
                <span className="ml-auto">Ctrl+S saves</span>
              </footer>
            </>
          )}
        </div>
      </div>

      {createOpen && (
        <Modal
          title="New artifact"
          onClose={() => setCreateOpen(false)}
          footer={
            <>
              <Button variant="ghost" onClick={() => setCreateOpen(false)}>
                Cancel
              </Button>
              <Button variant="primary" busy={creating} disabled={!createName.trim()} onClick={() => void create()}>
                Create
              </Button>
            </>
          }
        >
          <div className="space-y-4">
            <Field label="Name">
              <input
                autoFocus
                value={createName}
                onChange={(e) => setCreateName(e.target.value)}
                placeholder="Trip planner"
                maxLength={80}
                className={inputClass}
              />
            </Field>
            <Field label="Description" hint="Shown on the public gallery card.">
              <input
                value={createDescription}
                onChange={(e) => setCreateDescription(e.target.value)}
                placeholder="Plan a trip and keep the packing list"
                maxLength={500}
                className={inputClass}
              />
            </Field>
            <div className="space-y-1.5">
              <span className="text-xs font-medium text-slate-300">Starting point</span>
              <div className="grid gap-2 sm:grid-cols-3">
                {[...templates]
                  .sort((a, b) => TEMPLATE_ORDER.indexOf(a.id) - TEMPLATE_ORDER.indexOf(b.id))
                  .map((t) => (
                    <button
                      key={t.id}
                      type="button"
                      onClick={() => setCreateTemplate(t.id)}
                      aria-pressed={createTemplate === t.id}
                      className={`rounded-lg border px-3 py-2.5 text-left transition-colors ${
                        createTemplate === t.id
                          ? "border-emerald-500/50 bg-emerald-500/10"
                          : "border-line bg-canvas hover:border-slate-600"
                      }`}
                    >
                      <span className="block text-xs font-semibold text-slate-100">{t.name}</span>
                      <span className="mt-1 block text-label leading-snug text-slate-500">{t.description}</span>
                    </button>
                  ))}
              </div>
            </div>
            <p className="text-label leading-relaxed text-slate-500">
              The artifact is written to a folder in the Studio config directory. Nothing leaves this machine until you publish.
            </p>
          </div>
        </Modal>
      )}

      {siteOpen && (
        <Modal
          title="Artifact site"
          onClose={() => setSiteOpen(false)}
          footer={
            <Button variant="ghost" onClick={() => setSiteOpen(false)}>
              Close
            </Button>
          }
        >
          <div className="space-y-5">
            <div className="rounded-lg border border-line bg-canvas px-3 py-2.5 text-label leading-relaxed text-slate-400">
              One public repository holds the gallery and every published app at <span className="font-mono text-slate-300">a/&lt;slug&gt;/</span>.
              Drafts stay local. The first publish creates the repository if it does not exist.
            </div>

            <div className="space-y-2">
              <div className="flex items-center gap-2">
                <span className={`size-2 rounded-full ${siteReady ? "bg-emerald-400" : "bg-slate-600"}`} />
                <span className="text-xs font-medium text-slate-200">
                  {siteReady ? "GitHub Pages is on" : "Pages is not enabled yet"}
                </span>
                {site?.build_type && <Badge className="bg-white/5 text-slate-400 ring-white/10">{site.build_type}</Badge>}
                {site?.https_enforced && <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/25">HTTPS</Badge>}
              </div>
              {site?.url && (
                <button
                  type="button"
                  onClick={() => void studio.os.openExternal(site.url!).catch((e) => toast("error", errorText(e)))}
                  className="block max-w-full truncate font-mono text-label text-emerald-300 hover:underline"
                >
                  {site.url}
                </button>
              )}
              {site?.last_run && (
                <p className="flex items-center gap-1.5 text-label text-slate-500">
                  <CircleAlert className={`size-3.5 ${site.last_run.conclusion === "success" ? "text-emerald-400" : "text-amber-400"}`} />
                  Last deploy: {site.last_run.conclusion ?? site.last_run.status ?? "queued"}
                  {site.last_run.url && (
                    <button
                      type="button"
                      className="underline hover:text-slate-300"
                      onClick={() => void studio.os.openExternal(site.last_run!.url!).catch((e) => toast("error", errorText(e)))}
                    >
                      run log
                    </button>
                  )}
                </p>
              )}
              {siteError && <p className="text-label text-amber-300">{siteError}</p>}
            </div>

            <form
              className="space-y-2"
              onSubmit={(e) => {
                e.preventDefault();
                void saveSite();
              }}
            >
              <Field label="Site repository" hint="owner/name. It must belong to the signed-in GitHub account.">
                <input
                  value={repoDraft}
                  onChange={(e) => setRepoDraft(e.target.value)}
                  placeholder="konethegreat/agentos-artifacts"
                  className={inputClass}
                />
              </Field>
              <Button size="sm" type="submit" busy={savingSite} disabled={!repoDraft.trim()}>
                Save repository
              </Button>
            </form>

            <form
              className="space-y-2 border-t border-line pt-4"
              onSubmit={(e) => {
                e.preventDefault();
                void saveDomain();
              }}
            >
              <Field
                label="Custom domain"
                hint="Optional. Add a CNAME record for this host pointing at your GitHub Pages address first, then save here."
              >
                <input
                  value={domainDraft}
                  onChange={(e) => setDomainDraft(e.target.value)}
                  placeholder="artifacts.example.com"
                  className={inputClass}
                />
              </Field>
              <Button size="sm" type="submit" busy={savingDomain} disabled={!domainDraft.trim()}>
                Save domain
              </Button>
            </form>
          </div>
        </Modal>
      )}

      {confirmDelete && artifact && (
        <Modal
          title="Delete this artifact?"
          onClose={() => setConfirmDelete(false)}
          footer={
            <>
              <Button variant="ghost" onClick={() => setConfirmDelete(false)} autoFocus>
                Cancel
              </Button>
              <Button variant="danger" busy={deleting} onClick={() => void remove()}>
                Delete artifact
              </Button>
            </>
          }
        >
          <div className="space-y-2 text-sm leading-relaxed text-slate-300">
            <p>
              This removes <span className="font-semibold text-slate-100">{artifact.name}</span> and its folder from this machine.
            </p>
            <p className="text-slate-400">
              A published copy stays on the site until the next publish, and that publish removes it from the gallery.
            </p>
          </div>
        </Modal>
      )}
    </section>
  );
}
