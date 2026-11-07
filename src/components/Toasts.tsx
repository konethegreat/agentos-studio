import { CheckCircle2, CircleAlert, Info } from "lucide-react";
import { createContext, useCallback, useContext, useState, type ReactNode } from "react";

type Kind = "ok" | "error" | "info";
interface Toast {
  id: number;
  kind: Kind;
  text: string;
}

const ToastCtx = createContext<(kind: Kind, text: string) => void>(() => undefined);

export function useToast() {
  return useContext(ToastCtx);
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const push = useCallback((kind: Kind, text: string) => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t.slice(-3), { id, kind, text }]);
    window.setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "error" ? 9000 : 4500);
  }, []);
  const icon = { ok: <CheckCircle2 className="size-4 text-ok" />, error: <CircleAlert className="size-4 text-danger" />, info: <Info className="size-4 text-info" /> };
  return (
    <ToastCtx.Provider value={push}>
      {children}
      <div className="pointer-events-none fixed bottom-5 right-5 z-[60] flex w-96 flex-col gap-2">
        {toasts.map((t) => (
          <div key={t.id} className="animate-slide-in pointer-events-auto flex items-start gap-2.5 rounded-xl border border-line bg-card/95 px-4 py-3 text-sm text-slate-200 shadow-xl backdrop-blur">
            <span className="mt-0.5">{icon[t.kind]}</span>
            <span className="break-words">{t.text}</span>
          </div>
        ))}
      </div>
    </ToastCtx.Provider>
  );
}
