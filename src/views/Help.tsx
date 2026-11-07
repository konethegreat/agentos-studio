import { useEffect, useMemo, useState } from "react";
import { BookOpen, Search } from "lucide-react";
import { api, errorText } from "../lib/api";
import { useToast } from "../components/Toasts";
import { Markdown } from "../components/Markdown";

type Doc = { title: string; path: string; body: string };

/** The docs that ship with the build, searchable, so help cannot drift from the product. */
export function Help() {
  const toast = useToast();
  const [docs, setDocs] = useState<Doc[]>([]);
  const [q, setQ] = useState("");
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    void api
      .helpDocs()
      .then((d) => {
        setDocs(d);
        setSelected(d[0]?.path ?? null);
      })
      .catch((e) => toast("error", errorText(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
        <span className="text-[11px] text-slate-500">{docs.length} documents from docs/</span>
        <div className="relative ml-auto">
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
                className={`block w-full px-4 py-2 text-left text-[12.5px] ${
                  current?.path === d.path ? "bg-emerald-500/10 text-emerald-200" : "text-slate-300 hover:bg-white/[0.03]"
                }`}
              >
                {d.title}
                <span className="block font-mono text-[10px] text-slate-600">{d.path}</span>
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
