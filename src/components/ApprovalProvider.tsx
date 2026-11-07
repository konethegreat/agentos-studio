import { ShieldAlert } from "lucide-react";
import { createContext, useCallback, useContext, useRef, useState, type ReactNode } from "react";
import { errorText } from "../lib/api";
import { approvalRequest, studio } from "../lib/studio";
import { Button, Modal } from "./ui";

type Approved = { approved?: boolean };
type Guarded = <T>(call: (a: Approved) => Promise<T>) => Promise<T>;

const Ctx = createContext<Guarded>((call) => call({}));

/** Run a sensitive command; if Rust answers APPROVAL_REQUIRED, ask the user and retry. */
export function useGuarded(): Guarded {
  return useContext(Ctx);
}

export class ApprovalDeclined extends Error {
  constructor() {
    super("Cancelled");
  }
}

interface Pending {
  action: string;
  summary: string;
  resolve: (ok: boolean) => void;
}

export function ApprovalProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<Pending | null>(null);
  const [always, setAlways] = useState(false);
  const [busy, setBusy] = useState(false);
  const queue = useRef(Promise.resolve());

  const ask = useCallback((action: string, summary: string) => {
    const p = queue.current.then(
      () =>
        new Promise<boolean>((resolve) => {
          setAlways(false);
          setPending({ action, summary, resolve });
        }),
    );
    queue.current = p.then(() => undefined);
    return p;
  }, []);

  const guarded: Guarded = useCallback(
    async <T,>(call: (a: Approved) => Promise<T>): Promise<T> => {
      try {
        return await call({});
      } catch (e) {
        const req = approvalRequest(e);
        if (!req) throw e;
        const ok = await ask(req.action, req.summary);
        if (!ok) throw new ApprovalDeclined();
        return call({ approved: true });
      }
    },
    [ask],
  );

  async function decide(ok: boolean) {
    if (!pending) return;
    if (ok && always) {
      setBusy(true);
      try {
        await studio.safety.setPolicy(pending.action, "allow");
      } catch (e) {
        console.error(errorText(e));
      } finally {
        setBusy(false);
      }
    }
    pending.resolve(ok);
    setPending(null);
  }

  return (
    <Ctx.Provider value={guarded}>
      {children}
      {pending && (
        <Modal
          title="Approve this action?"
          onClose={() => void decide(false)}
          footer={
            <>
              <Button variant="ghost" onClick={() => void decide(false)} autoFocus>
                Cancel
              </Button>
              <Button variant="primary" busy={busy} onClick={() => void decide(true)}>
                Approve and run
              </Button>
            </>
          }
        >
          <div className="flex gap-3">
            <ShieldAlert className="mt-0.5 size-5 shrink-0 text-amber-300" />
            <div className="min-w-0 space-y-3">
              <p className="text-sm leading-relaxed text-slate-200">{pending.summary}</p>
              <p className="font-mono text-label text-slate-500">Permission: {pending.action}</p>
              <label className="flex items-center gap-2 text-xs text-slate-400" title="Sets this action to Allow in Safety. You can change it back there at any time.">
                <input type="checkbox" checked={always} onChange={(e) => setAlways(e.target.checked)} className="accent-emerald-500" />
                Always allow this kind of action
              </label>
            </div>
          </div>
        </Modal>
      )}
    </Ctx.Provider>
  );
}
