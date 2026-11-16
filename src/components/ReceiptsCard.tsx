import { useCallback, useEffect, useState } from "react";
import { FileCheck2, Loader2, ShieldAlert, ShieldCheck } from "lucide-react";
import { api, errorText, type MergeReceipt, type ReceiptVerification } from "../lib/api";
import { Badge, Button, Card } from "./ui";

function when(at: number): string {
  return new Date(at * 1000).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/**
 * Proof of every merge (docs/BEYOND.md, B5): each merge the gate makes leaves a receipt with
 * what it saw, hash-chained and signed with a key made on this machine.
 */
export function ReceiptsCard() {
  const [rows, setRows] = useState<MergeReceipt[] | null>(null);
  const [check, setCheck] = useState<ReceiptVerification | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    try {
      setRows(await api.receipts(20));
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function verify() {
    setBusy(true);
    try {
      setCheck(await api.verifyReceipts());
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className="space-y-3 p-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <div className="flex items-center gap-2 text-base font-semibold text-slate-100">
            <FileCheck2 className="size-5" /> Proof of every merge
          </div>
          <p className="mt-1 max-w-3xl text-small text-muted">
            Each merge the gate makes leaves a receipt: the reviewed commit, the merge commit, every CI check, who did
            the work and who approved it. Receipts are chained and signed on this machine, so changing or deleting any
            of them is caught.
          </p>
        </div>
        <Button size="sm" busy={busy} icon={busy ? <Loader2 className="size-3.5" /> : <ShieldCheck className="size-3.5" />} onClick={() => void verify()}>
          Verify the chain
        </Button>
      </div>
      {error && <div className="text-label text-rose-300">{error}</div>}
      {check && (
        <div
          className={`flex items-center gap-2 rounded-lg px-3 py-2 text-small ring-1 ${
            check.ok ? "bg-emerald-500/[0.06] text-emerald-200 ring-emerald-500/25" : "bg-rose-500/[0.06] text-rose-200 ring-rose-500/25"
          }`}
        >
          {check.ok ? <ShieldCheck className="size-4" /> : <ShieldAlert className="size-4" />}
          {check.ok
            ? `All ${check.receipts} receipts verify: every link, hash and signature holds.`
            : `Verification failed: ${check.problem}`}
        </div>
      )}
      {rows === null ? (
        <p className="text-small text-slate-500">Reading receipts...</p>
      ) : rows.length === 0 ? (
        <p className="text-small text-slate-500">No receipts yet. The next merge the gate makes writes the first one.</p>
      ) : (
        <div className="space-y-1">
          {rows.map((r) => (
            <div key={r.seq} className="flex flex-wrap items-center gap-2 text-small">
              <span className="w-8 text-right font-mono text-label text-slate-500">#{r.seq}</span>
              <span className="font-mono text-label text-slate-300">{r.mission}</span>
              <span className="text-slate-200">{r.repo}</span>
              <a className="font-mono text-label text-sky-300 hover:underline" href={r.pr_url} target="_blank" rel="noreferrer">
                PR {r.pr}
              </a>
              <span className="text-label text-slate-500">
                {r.implementer}, reviewed by {r.reviewer}
              </span>
              <Badge
                className={
                  r.checks.length > 0 && r.checks.every((c) => ["SUCCESS", "SKIPPED", "NEUTRAL"].includes(c.state))
                    ? "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25"
                    : "bg-amber-500/10 text-amber-300 ring-amber-500/25"
                }
              >
                {r.checks.length} checks
              </Badge>
              <span className="font-mono text-label text-slate-500" title={r.hash}>
                {r.hash.slice(0, 10)}
              </span>
              <span className="ml-auto text-label text-slate-500">{when(r.at)}</span>
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}
