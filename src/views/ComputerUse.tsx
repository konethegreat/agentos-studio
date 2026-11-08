import { Image as ImageIcon, Loader2, MousePointer2, RefreshCw, Wrench } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { ApprovalDeclined, useGuarded } from "../components/ApprovalProvider";
import { useCli } from "../components/CliDrawer";
import { useToast } from "../components/Toasts";
import { Badge, Button, Card } from "../components/ui";
import { errorText } from "../lib/api";
import { studio, type ComputerAction, type ComputerUseStatus } from "../lib/studio";

/** What Hermes did on the desktop, the cua-driver behind it, and the switch for it. */
export function ComputerUse() {
  const toast = useToast();
  const guarded = useGuarded();
  const cli = useCli();
  const [status, setStatus] = useState<ComputerUseStatus | null>(null);
  const [acts, setActs] = useState<ComputerAction[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [shot, setShot] = useState<{ path: string; url: string | null; error?: string } | null>(null);

  const load = useCallback(() => {
    void studio.computer.status().then(setStatus).catch((e) => toast("error", errorText(e)));
    void studio.computer.activity(80).then(setActs).catch((e) => {
      setActs([]);
      toast("error", errorText(e));
    });
  }, [toast]);
  useEffect(() => {
    load();
  }, [load]);

  async function toggle(on: boolean) {
    setBusy(true);
    try {
      await guarded((a) => studio.computer.setEnabled(on, a));
      toast("ok", on ? "Hermes may use the computer" : "Computer use is off for Hermes");
      load();
    } catch (e) {
      if (!(e instanceof ApprovalDeclined)) toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  function open(path: string) {
    setShot({ path, url: null });
    void studio.computer
      .screenshot(path)
      .then((url) => setShot({ path, url }))
      .catch((e) => setShot({ path, url: null, error: errorText(e) }));
  }

  const enabled = status?.toolset_enabled;
  return (
    <div className="h-full overflow-y-auto p-6">
      <div className="mb-4 flex items-center gap-3">
        <div className="grid size-9 place-items-center rounded-lg bg-emerald-500/10 ring-1 ring-emerald-500/25">
          <MousePointer2 className="size-4 text-emerald-300" />
        </div>
        <div className="flex-1">
          <h1 className="text-base font-semibold text-slate-100">Computer Use</h1>
          <p className="text-small text-slate-500">Hermes can see the screen and use the mouse and keyboard through cua-driver. This is what it did, and the switch for it.</p>
        </div>
        <Button size="sm" icon={<RefreshCw className="size-3.5" />} onClick={load}>
          Refresh
        </Button>
      </div>

      <Card className="mb-4 p-5">
        {!status && <Loader2 className="size-4 animate-spin text-slate-500" />}
        {status && (
          <div className="flex flex-wrap items-center gap-x-6 gap-y-3 text-small">
            <div className="flex items-center gap-2">
              <span className="text-slate-400">Toolset</span>
              {enabled === null ? (
                <Badge className="bg-white/5 text-slate-400 ring-white/10">unknown</Badge>
              ) : enabled ? (
                <Badge className="bg-emerald-500/10 text-emerald-300 ring-emerald-500/25">on</Badge>
              ) : (
                <Badge className="bg-white/5 text-slate-400 ring-white/10">off</Badge>
              )}
              {enabled !== null && (
                <Button size="sm" busy={busy} onClick={() => void toggle(!enabled)}>
                  {enabled ? "Turn off" : "Turn on"}
                </Button>
              )}
            </div>
            <div className="flex items-center gap-2">
              <span className="text-slate-400">cua-driver</span>
              {status.driver_path ? (
                <span className="font-mono text-slate-200" title={status.driver_path}>
                  {status.driver_version ?? "installed"}
                </span>
              ) : (
                <span className="text-amber-300">not installed</span>
              )}
              {status.update_available && (
                <Badge className="bg-sky-500/10 text-sky-300 ring-sky-500/25">{status.latest_version ?? "update"} available</Badge>
              )}
            </div>
            {status.daemon && <span className="text-slate-500">{status.daemon} (Hermes starts it when needed)</span>}
            <div className="ml-auto flex gap-2">
              {!status.driver_path && (
                <Button size="sm" variant="primary" icon={<Wrench className="size-3.5" />} onClick={() => void cli.run("hermes tools post-setup cua_driver")}>
                  Install cua-driver
                </Button>
              )}
              {status.driver_path && status.update_available && (
                <Button size="sm" onClick={() => void cli.run(`"${status.driver_path}" update --apply`)}>
                  Update driver
                </Button>
              )}
              {status.driver_path && (
                <Button size="sm" onClick={() => void cli.run(`"${status.driver_path}" doctor`)}>
                  Run doctor
                </Button>
              )}
            </div>
          </div>
        )}
      </Card>

      <h2 className="mb-2 text-sm font-semibold text-slate-100">Recent actions</h2>
      {!acts && <Loader2 className="size-4 animate-spin text-slate-500" />}
      {acts?.length === 0 && <p className="text-small text-slate-500">Hermes has not used the computer yet.</p>}
      {acts && acts.length > 0 && (
        <Card className="divide-y divide-line">
          {acts.map((a, i) => (
            <div key={`${a.session_id}-${a.at}-${i}`} className="flex items-center gap-3 px-4 py-2 text-small">
              <span className="w-36 shrink-0 text-slate-500">{new Date(a.at * 1000).toLocaleString()}</span>
              <span className={`w-28 shrink-0 font-mono ${a.ok === false ? "text-rose-300" : "text-slate-200"}`}>{a.action}</span>
              <span className="min-w-0 flex-1 truncate text-slate-400" title={a.summary}>
                {a.summary}
              </span>
              <span className="hidden w-32 shrink-0 truncate font-mono text-label text-slate-600 lg:inline" title={a.session_id}>
                {a.session_id.replace(/^local-/, "").slice(0, 10)}
              </span>
              {a.screenshot ? (
                <Button size="sm" variant="ghost" icon={<ImageIcon className="size-3.5" />} onClick={() => open(a.screenshot!)} aria-label="Show screenshot">
                  Screenshot
                </Button>
              ) : (
                <span className="w-24 shrink-0" />
              )}
            </div>
          ))}
        </Card>
      )}

      {shot && (
        <div className="fixed inset-0 z-50 grid place-items-center bg-black/70 p-8" onClick={() => setShot(null)} role="dialog" aria-label="Screenshot">
          <div className="max-h-full max-w-full overflow-auto rounded-lg bg-inset p-2 ring-1 ring-line" onClick={(e) => e.stopPropagation()}>
            {shot.url && <img src={shot.url} alt="What Hermes saw" className="max-h-[80vh] max-w-[85vw] object-contain" />}
            {!shot.url && !shot.error && <Loader2 className="m-8 size-5 animate-spin text-slate-500" />}
            {shot.error && <p className="m-6 text-small text-rose-300">{shot.error}</p>}
            <p className="mt-2 truncate px-1 font-mono text-label text-slate-500">{shot.path}</p>
          </div>
        </div>
      )}
    </div>
  );
}
