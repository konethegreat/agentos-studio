import { Loader2, ShieldAlert } from "lucide-react";
import { useState } from "react";
import { errorText } from "../lib/api";
import { studio, type AuditFinding } from "../lib/studio";
import { useToast } from "./Toasts";
import { Badge, Button, Card } from "./ui";

const SEVERITY: Record<string, string> = {
  CRITICAL: "bg-rose-500/10 text-rose-300 ring-rose-500/25",
  HIGH: "bg-orange-500/10 text-orange-300 ring-orange-500/25",
  MODERATE: "bg-amber-500/10 text-amber-300 ring-amber-500/25",
  LOW: "bg-white/5 text-slate-300 ring-white/10",
};

/** `hermes security audit --json`: OSV.dev findings for the Hermes venv, plugins and pinned MCP
 *  servers. Fixes are Hermes updates (Updates screen), not something Studio patches. */
export function HermesAuditCard() {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ scanned: number; findings: AuditFinding[] } | null>(null);
  async function run() {
    setBusy(true);
    try {
      const r = await studio.hermes.securityAudit();
      setResult({ scanned: r.total_components_scanned ?? 0, findings: r.findings ?? [] });
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Card className="p-0">
      <div className="flex items-start gap-3 border-b border-line px-4 py-3">
        <div className="flex-1">
          <h2 className="text-sm font-semibold text-slate-100">Hermes supply-chain audit</h2>
          <p className="mt-0.5 text-small text-slate-500">
            Runs hermes security audit: known vulnerabilities (OSV.dev) in the Hermes Python environment, plugin dependencies and pinned MCP servers. It needs the network and takes a few seconds.
          </p>
        </div>
        <Button size="sm" variant="primary" busy={busy} icon={busy ? <Loader2 className="size-3.5 animate-spin" /> : <ShieldAlert className="size-3.5" />} onClick={() => void run()}>
          {result ? "Scan again" : "Scan"}
        </Button>
      </div>
      {result && (
        <div className="px-4 py-3">
          <p className="mb-2 text-small text-slate-400">
            {result.scanned} components scanned, {result.findings.length} finding{result.findings.length === 1 ? "" : "s"}.
            {result.findings.length > 0 && " Update Hermes from the Updates screen; the fixed version is listed per finding."}
          </p>
          {result.findings.length > 0 && (
            <ul className="divide-y divide-line rounded-lg ring-1 ring-line">
              {result.findings.map((f) => (
                <li key={f.vuln_id + f.package} className="px-3 py-2 text-small">
                  <div className="flex items-center gap-2">
                    <Badge className={SEVERITY[f.severity?.toUpperCase()] ?? SEVERITY.LOW}>{f.severity}</Badge>
                    <span className="font-mono text-slate-200">
                      {f.package} {f.version}
                    </span>
                    <span className="text-label text-slate-500">{f.source}</span>
                    <span className="ml-auto font-mono text-label text-slate-500">{f.vuln_id}</span>
                  </div>
                  <div className="mt-0.5 text-slate-400">{f.summary}</div>
                  {f.fixed_versions?.length > 0 && <div className="text-label text-emerald-300">fixed in {f.fixed_versions.join(", ")}</div>}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </Card>
  );
}
