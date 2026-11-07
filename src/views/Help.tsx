import { useEffect, useMemo, useState } from "react";
import { BookOpen, Lightbulb, Search } from "lucide-react";
import { api, errorText } from "../lib/api";
import { useToast } from "../components/Toasts";
import { Markdown } from "../components/Markdown";
import { useGuides } from "../components/Guides";

type Doc = { title: string; path: string; body: string };

/** The docs that ship with the build, searchable, so help cannot drift from the product. */
export function Help({ doc }: { doc?: { path: string; nonce: number } | null }) {
  const toast = useToast();
  const guides = useGuides();
  const [docs, setDocs] = useState<Doc[]>([]);
  const [q, setQ] = useState("");
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    void api
      .helpDocs()
      .then((d) => {
        setDocs(d);
        setSelected((cur) => cur ?? d[0]?.path ?? null);
      })
      .catch((e) => toast("error", errorText(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Opened from a guide's "Read more": select that document and clear any search hiding it.
  useEffect(() => {
    if (!doc) return;
    setQ("");
    setSelected(doc.path);
  }, [doc?.nonce]);

  const filtered = useMemo(() => {
    const query = q.trim().toLowerCase();
    if (!query) return docs;
    return docs.filter((d) => d.title.toLowerCase().includes(query) || d.body.toLowerCase().includes(query));
  }, [docs, q]);

  const current = filtered.find((d) => d.path === selected) ?? filtered[0];

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-3 border-b border-line px-4 py-3">
        <BookOpen className="size-4 text-emerald-300" />
        <div className="text-sm font-semibold text-slate-100">Help</div>
        <span className="text-label text-slate-500">{docs.length} documents from docs/</span>
        <div className="ml-auto flex items-center gap-2 text-label text-slate-500">
          <Lightbulb className="size-3.5" aria-hidden />
          <label className="inline-flex cursor-pointer items-center gap-1.5" title="Short hints that appear when they apply to what you are doing">
            <input type="checkbox" checked={!guides.off} onChange={(e) => guides.setOff(!e.target.checked)} className="size-3 accent-emerald-500" />
            Tips
          </label>
          <button
            onClick={() => {
              guides.reset();
              void api.setPref("onboarding.done", "0").catch(() => undefined);
              toast("ok", "Tips and the first-run guide will show again");
            }}
            disabled={guides.dismissedCount === 0 && !guides.off}
            className="rounded-md px-1.5 py-0.5 ring-1 ring-line hover:text-slate-300 disabled:opacity-40"
          >
            Show tips again
          </button>
        </div>
        <div className="relative">
          <Search className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-slate-500" />
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Search the docs"
            className="h-8 w-64 rounded-lg bg-white/[0.03] pl-7 pr-2 text-xs text-slate-200 outline-none ring-1 ring-line focus:ring-emerald-500/40"
          />
        </div>
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-1 divide-y divide-line lg:grid-cols-[260px_1fr] lg:divide-x lg:divide-y-0">
        <ul className="min-h-0 overflow-y-auto">
          {filtered.length === 0 && <li className="p-4 text-xs text-slate-500">Nothing matches.</li>}
          {filtered.map((d) => (
            <li key={d.path}>
              <button
                onClick={() => setSelected(d.path)}
                className={`block w-full px-4 py-2 text-left text-body-sm ${
                  current?.path === d.path ? "bg-emerald-500/10 text-emerald-200" : "text-slate-300 hover:bg-white/[0.03]"
                }`}
              >
                {d.title}
                <span className="block font-mono text-2xs text-slate-600">{d.path}</span>
              </button>
            </li>
          ))}
        </ul>
        <div className="min-h-0 overflow-y-auto px-6 py-5">
          {current ? <Markdown text={current.body} workingDir="" /> : <div className="text-sm text-slate-500">No document selected.</div>}
        </div>
      </div>
    </div>
  );
}
