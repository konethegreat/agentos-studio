import { ArrowUpCircle, BellOff, Clock, X } from "lucide-react";
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { errorText } from "../lib/api";
import { studio, studioEvents, type BackupManifest, type Product, type ProductUpdate, type UpdateProgress } from "../lib/studio";
import { ApprovalDeclined, useGuarded } from "./ApprovalProvider";
import { useToast } from "./Toasts";

interface UpdatesApi {
  products: ProductUpdate[];
  checkedAt: number | null;
  checking: boolean;
  check: () => Promise<void>;
  apply: (p: Product) => Promise<void>;
  skip: (p: ProductUpdate) => Promise<void>;
  snooze: (p: Product, hours: number) => Promise<void>;
  clear: (p: Product) => Promise<void>;
  rollback: (b: BackupManifest) => Promise<void>;
  backups: BackupManifest[];
  log: UpdateProgress[];
  busy: Product | null;
  alerts: ProductUpdate[];
}

const Ctx = createContext<UpdatesApi | null>(null);

export function useUpdates(): UpdatesApi {
  const v = useContext(Ctx);
  if (!v) throw new Error("useUpdates outside UpdatesProvider");
  return v;
}

export function UpdatesProvider({ children }: { children: ReactNode }) {
  const toast = useToast();
  const guarded = useGuarded();
  const [products, setProducts] = useState<ProductUpdate[]>([]);
  const [checkedAt, setCheckedAt] = useState<number | null>(null);
  const [checking, setChecking] = useState(false);
  const [backups, setBackups] = useState<BackupManifest[]>([]);
  const [log, setLog] = useState<UpdateProgress[]>([]);
  const [busy, setBusy] = useState<Product | null>(null);

  const loadBackups = useCallback(() => void studio.updates.backups().then(setBackups).catch(() => undefined), []);

  const check = useCallback(async () => {
    setChecking(true);
    try {
      const r = await studio.updates.check();
      setProducts(r.products);
      setCheckedAt(r.checked_at);
    } catch (e) {
      toast("error", `Update check failed: ${errorText(e)}`);
    } finally {
      setChecking(false);
    }
  }, [toast]);

  useEffect(() => {
    void studio.updates
      .last()
      .then((p) => {
        if (p && p.length) setProducts(p);
      })
      .catch(() => undefined);
    loadBackups();
    const un1 = studioEvents.updates((r) => {
      setProducts(r.products);
      setCheckedAt(r.checked_at);
    });
    const un2 = studioEvents.updateProgress((e) => {
      setLog((l) => [...l.slice(-1500), e]);
      if (e.done) {
        setBusy(null);
        toast(e.ok ? "ok" : "error", e.line);
        loadBackups();
        void check();
      }
    });
    return () => {
      void un1.then((f) => f());
      void un2.then((f) => f());
    };
  }, [check, loadBackups, toast]);

  const apply = useCallback(
    async (p: Product) => {
      try {
        setLog([]);
        setBusy(p);
        await guarded((a) => studio.updates.apply(p, a));
      } catch (e) {
        setBusy(null);
        if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
      }
    },
    [guarded, toast],
  );

  const rollback = useCallback(
    async (b: BackupManifest) => {
      try {
        setLog([]);
        setBusy(b.product);
        await guarded((a) => studio.updates.rollback(b.id, a));
      } catch (e) {
        setBusy(null);
        if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
      }
    },
    [guarded, toast],
  );

  const skip = useCallback(
    async (p: ProductUpdate) => {
      if (!p.latest) return;
      await studio.updates.skip(p.product, p.latest).catch((e) => toast("error", errorText(e)));
      setProducts((list) => list.map((x) => (x.product === p.product ? { ...x, skipped: true, alert: false } : x)));
      toast("info", `${p.label} ${p.latest} skipped. You will be told about the next version.`);
    },
    [toast],
  );

  const snooze = useCallback(
    async (p: Product, hours: number) => {
      try {
        const until = await studio.updates.snooze(p, hours);
        setProducts((list) => list.map((x) => (x.product === p ? { ...x, snoozed_until: until, alert: false } : x)));
      } catch (e) {
        toast("error", errorText(e));
      }
    },
    [toast],
  );

  const clear = useCallback(
    async (p: Product) => {
      await studio.updates.clear(p).catch((e) => toast("error", errorText(e)));
      await check();
    },
    [check, toast],
  );

  const alerts = useMemo(() => products.filter((p) => p.alert), [products]);

  return (
    <Ctx.Provider value={{ products, checkedAt, checking, check, apply, skip, snooze, clear, rollback, backups, log, busy, alerts }}>{children}</Ctx.Provider>
  );
}

/** The in-app alert shown above every view while an update is waiting. */
export function UpdateBanner({ onOpen }: { onOpen: () => void }) {
  const u = useUpdates();
  const [hidden, setHidden] = useState<string>("");
  const first = u.alerts[0];
  const key = u.alerts.map((a) => `${a.product}@${a.latest}`).join(",");
  if (!first || hidden === key) return null;
  const more = u.alerts.length - 1;
  return (
    <div className="flex shrink-0 items-center gap-3 border-b border-emerald-500/20 bg-emerald-500/[0.07] px-5 py-2 text-xs" role="status">
      <ArrowUpCircle className="size-4 shrink-0 text-emerald-300" />
      <div className="min-w-0 flex-1 truncate text-slate-200">
        <span className="font-semibold text-emerald-200">
          {first.label} {first.latest}
        </span>{" "}
        is available (you have {first.current}).{" "}
        <span className="text-slate-400">{first.changelog?.highlights[0] ?? first.detail}</span>
        {more > 0 && <span className="text-slate-400"> Plus {more} more.</span>}
      </div>
      <button onClick={onOpen} className="rounded-md bg-emerald-500 px-2.5 py-1 font-semibold text-emerald-950 hover:bg-emerald-400" title="Open the Update Center to read the changes and update">
        Review update
      </button>
      <button onClick={() => void u.snooze(first.product, 24)} className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-slate-300 hover:bg-white/5" title="Hide this alert for 24 hours">
        <Clock className="size-3.5" /> Remind me later
      </button>
      <button onClick={() => void u.skip(first)} className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-slate-300 hover:bg-white/5" title="Do not alert again for this version">
        <BellOff className="size-3.5" /> Skip this version
      </button>
      <button onClick={() => setHidden(key)} className="rounded p-1 text-slate-500 hover:bg-white/5 hover:text-slate-200" title="Hide until next launch" aria-label="Dismiss">
        <X className="size-3.5" />
      </button>
    </div>
  );
}
