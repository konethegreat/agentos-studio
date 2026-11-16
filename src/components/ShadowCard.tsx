import { useCallback, useEffect, useState } from "react";
import { UserRoundCog } from "lucide-react";
import { api, errorText, type ShadowRecord, type ShadowStatus } from "../lib/api";
import { Badge, Card, Toggle, inputClass } from "./ui";
import { useToast } from "./Toasts";

const OUTCOME: Record<ShadowRecord["outcome"], { text: string; tone: string }> = {
  pending: { text: "waiting for the window", tone: "bg-sky-500/10 text-sky-300 ring-sky-500/25" },
  answered: { text: "Shadow answered", tone: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25" },
  agreed: { text: "you answered the same", tone: "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25" },
  overruled: { text: "you overruled it", tone: "bg-amber-500/10 text-amber-300 ring-amber-500/25" },
  expired: { text: "no longer open", tone: "bg-slate-500/10 text-slate-400 ring-slate-500/25" },
};

function at(epoch: number): string {
  return new Date(epoch * 1000).toLocaleString(undefined, { weekday: "short", hour: "2-digit", minute: "2-digit" });
}

/**
 * Shadow Kone (docs/SHADOW.md): answers the questions the owner has already answered the same way
 * three times in a row, or has a standing answer for, after a window in which he can overrule it.
 */
export function ShadowCard() {
  const toast = useToast();
  const [s, setS] = useState<ShadowStatus | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setS(await api.shadowStatus());
    } catch (e) {
      toast("error", errorText(e));
    }
  }, [toast]);

  useEffect(() => {
    void load();
  }, [load]);

  async function change(p: Parameters<typeof api.shadowSet>[0]) {
    setBusy(true);
    try {
      setS(await api.shadowSet(p));
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  if (!s) return null;
  const decided = s.record.filter((r) => r.outcome === "answered").length;

  return (
    <Card className="space-y-3 p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <div className="flex items-center gap-2 text-base font-semibold text-slate-100">
            <UserRoundCog className="size-5" /> Shadow: answers you have already given
          </div>
          <p className="mt-1 max-w-3xl text-small text-muted">
            When you have answered the same kind of question the same way three times in a row, or set a standing answer
            below, Shadow answers it for you after a window. It tells you on Today and on your phone first, and your own
            answer inside the window always wins. It never answers a budget, a destructive approval or a merge outside
            the allow list.
          </p>
        </div>
        <div className="flex items-center gap-4">
          <Toggle checked={s.enabled} label={s.enabled ? "On" : "Off"} onChange={(on) => void change({ enabled: on })} />
          <label className="flex items-center gap-2 whitespace-nowrap text-small text-slate-300">
            Window
            <select
              className={`${inputClass} !w-24`}
              value={s.window_minutes}
              disabled={busy}
              onChange={(e) => void change({ windowMinutes: Number(e.target.value) })}
            >
              {[2, 5, 10, 15, 30, 45].map((n) => (
                <option key={n} value={n}>
                  {n} min
                </option>
              ))}
            </select>
          </label>
        </div>
      </div>

      <div className="flex flex-wrap gap-3 text-small text-slate-300">
        <span>{decided} answered by Shadow</span>
        <span>
          {s.agreement === null
            ? "no answer of yours to compare yet"
            : `${Math.round(s.agreement * 100)}% agreement when you answered first`}
        </span>
      </div>

      <div className="overflow-x-auto">
        <table className="w-full text-small">
          <thead>
            <tr className="text-left text-label text-muted">
              <th className="py-1 pr-3 font-normal">Question</th>
              <th className="py-1 pr-3 font-normal">Standing answer</th>
              <th className="py-1 pr-3 font-normal">Learned from you</th>
              <th className="py-1 font-normal">Your recent answers</th>
            </tr>
          </thead>
          <tbody>
            {s.rules.map((r) => (
              <tr key={r.class} className="border-t border-line">
                <td className="py-1.5 pr-3 text-slate-200">{r.label}</td>
                <td className="py-1.5 pr-3">
                  {r.never ? (
                    <span className="text-label text-muted">always yours</span>
                  ) : (
                    <select
                      className={`${inputClass} !w-28`}
                      aria-label={`Standing answer for: ${r.label}`}
                      value={r.standing ?? ""}
                      disabled={busy}
                      onChange={(e) => void change({ cls: r.class, answer: e.target.value })}
                    >
                      <option value="">none</option>
                      <option value="retry">retry</option>
                      <option value="stop">stop</option>
                    </select>
                  )}
                </td>
                <td className="py-1.5 pr-3">
                  {r.never ? null : r.learned ? (
                    <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/25">{r.learned}</Badge>
                  ) : (
                    <span className="text-label text-muted">not yet</span>
                  )}
                </td>
                <td className="py-1.5 font-mono text-label text-slate-400">{r.precedents.join(", ") || "none"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {s.record.length > 0 && (
        <div className="space-y-1">
          <div className="text-label text-muted">What Shadow planned</div>
          {s.record.slice(0, 8).map((r) => (
            <div key={`${r.escalation}-${r.due_at}`} className="flex flex-wrap items-center gap-2 text-small">
              <span className="font-mono text-label text-slate-500">{r.escalation}</span>
              <span className="text-slate-200">{r.planned}</span>
              <Badge className={OUTCOME[r.outcome].tone}>{OUTCOME[r.outcome].text}</Badge>
              {r.outcome === "overruled" && r.final_answer && <span className="text-label text-slate-400">you said {r.final_answer}</span>}
              <span className="text-label text-slate-500">{at(r.due_at)}</span>
              <span className="min-w-0 flex-1 truncate text-label text-slate-500" title={r.reason}>
                {r.reason}
              </span>
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}
