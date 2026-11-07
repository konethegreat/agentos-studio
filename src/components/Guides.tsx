import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { BookOpen, Lightbulb, X } from "lucide-react";
import { api } from "../lib/api";
import { PREF_DISMISSED, PREF_OFF, guideById, parseDismissed, pickGuide, serializeDismissed, type GuideId } from "../lib/guides";

type Ctx = {
  shown: GuideId | null;
  off: boolean;
  /** A view reports which of its guides currently apply; `owner` scopes the list to that view. */
  report: (owner: string, ids: GuideId[]) => void;
  dismiss: (id: GuideId) => void;
  /** The user did what the guide suggests, so it has served its purpose. */
  done: (id: GuideId) => void;
  setOff: (off: boolean) => void;
  reset: () => void;
  dismissedCount: number;
};

const GuideContext = createContext<Ctx | null>(null);

/** Opens a Help document from anywhere; App listens and switches to the Help view. */
export function openHelpDoc(path: string) {
  window.dispatchEvent(new CustomEvent("agentos:open-help", { detail: path }));
}

export function GuideProvider({ children }: { children: ReactNode }) {
  const [loaded, setLoaded] = useState(false);
  const [dismissed, setDismissed] = useState<Set<GuideId>>(new Set());
  const [off, setOffState] = useState(false);
  const [byOwner, setByOwner] = useState<Record<string, GuideId[]>>({});

  useEffect(() => {
    void Promise.all([api.getPref(PREF_DISMISSED).catch(() => null), api.getPref(PREF_OFF).catch(() => null)]).then(([d, o]) => {
      setDismissed(parseDismissed(d));
      setOffState(o === "1");
      setLoaded(true);
    });
  }, []);

  const persist = useCallback((next: Set<GuideId>) => {
    void api.setPref(PREF_DISMISSED, serializeDismissed(next)).catch(() => undefined);
  }, []);

  const dismiss = useCallback(
    (id: GuideId) => {
      setDismissed((cur) => {
        if (cur.has(id)) return cur;
        const next = new Set(cur).add(id);
        persist(next);
        return next;
      });
    },
    [persist],
  );

  const report = useCallback((owner: string, ids: GuideId[]) => {
    setByOwner((cur) => {
      const prev = cur[owner] ?? [];
      if (prev.length === ids.length && prev.every((x, i) => x === ids[i])) return cur;
      return { ...cur, [owner]: ids };
    });
  }, []);

  const setOff = useCallback((v: boolean) => {
    setOffState(v);
    void api.setPref(PREF_OFF, v ? "1" : "0").catch(() => undefined);
  }, []);

  const reset = useCallback(() => {
    const empty = new Set<GuideId>();
    setDismissed(empty);
    persist(empty);
    setOff(false);
  }, [persist, setOff]);

  const shown = useMemo(
    () => (loaded ? pickGuide(Object.values(byOwner).flat(), dismissed, off) : null),
    [loaded, byOwner, dismissed, off],
  );

  const value = useMemo<Ctx>(
    () => ({ shown, off, report, dismiss, done: dismiss, setOff, reset, dismissedCount: dismissed.size }),
    [shown, off, report, dismiss, setOff, reset, dismissed.size],
  );
  return <GuideContext.Provider value={value}>{children}</GuideContext.Provider>;
}

export function useGuides(): Ctx {
  const c = useContext(GuideContext);
  if (!c) throw new Error("useGuides must be used inside GuideProvider");
  return c;
}

/**
 * Report the guides that apply to one view right now. The list is cleared when the view unmounts
 * or hides, so a guide never shows for a screen the user is not looking at.
 */
export function useReportGuides(owner: string, ids: GuideId[], visible = true) {
  const { report } = useGuides();
  const key = visible ? ids.join(",") : "";
  const latest = useRef(ids);
  latest.current = ids;
  useEffect(() => {
    report(owner, visible ? latest.current : []);
  }, [owner, key, visible, report]);
  useEffect(() => {
    return () => report(owner, []);
  }, [owner, report]);
}

/**
 * The inline hint for one guide. Renders only when the provider picked this guide, so several
 * can sit in the tree and at most one is ever visible.
 */
export function Guide({ id, action, className = "" }: { id: GuideId; action?: { label: string; run: () => void }; className?: string }) {
  const { shown, dismiss, done } = useGuides();
  if (shown !== id) return null;
  const g = guideById(id);
  if (!g) return null;
  return (
    <div role="note" aria-label={`Tip: ${g.title}`} className={`flex items-start gap-2.5 rounded-xl bg-sky-500/[0.07] px-3 py-2.5 text-ui ring-1 ring-sky-500/25 ${className}`}>
      <Lightbulb className="mt-0.5 size-4 shrink-0 text-sky-300" aria-hidden />
      <div className="min-w-0 flex-1">
        <div className="font-medium text-sky-100">{g.title}</div>
        <p className="mt-0.5 leading-relaxed text-slate-400">{g.body}</p>
        <div className="mt-1.5 flex flex-wrap items-center gap-3">
          {action && (
            <button
              onClick={() => {
                action.run();
                done(id);
              }}
              className="text-small font-medium text-sky-300 hover:text-sky-200"
            >
              {action.label}
            </button>
          )}
          {g.doc && (
            <button onClick={() => openHelpDoc(g.doc!)} className="inline-flex items-center gap-1 text-small text-slate-400 hover:text-slate-200">
              <BookOpen className="size-3" aria-hidden /> Read more
            </button>
          )}
          <button onClick={() => dismiss(id)} className="text-small text-slate-500 hover:text-slate-300">
            Got it
          </button>
        </div>
      </div>
      <button onClick={() => dismiss(id)} aria-label="Dismiss tip" title="Dismiss" className="grid size-6 shrink-0 place-items-center rounded-md text-slate-500 hover:bg-white/5 hover:text-slate-300">
        <X className="size-3.5" />
      </button>
    </div>
  );
}
