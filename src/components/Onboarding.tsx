import { useEffect, useState } from "react";
import { ArrowRight, Check, KeyRound, LayoutDashboard, Sparkles } from "lucide-react";
import { api } from "../lib/api";

/**
 * First run: three things that actually matter, in the order they unblock work.
 * It is shown once and can be reopened from Help by clearing the preference.
 */
export function Onboarding({ onGo }: { onGo: (view: "setup" | "mission" | "help") => void }) {
  const [show, setShow] = useState(false);
  const [step, setStep] = useState(0);

  useEffect(() => {
    void api
      .getPref("onboarding.done")
      .then((v) => setShow(v !== "1"))
      .catch(() => undefined);
  }, []);

  function finish() {
    setShow(false);
    void api.setPref("onboarding.done", "1").catch(() => undefined);
  }

  if (!show) return null;

  const steps = [
    {
      icon: <KeyRound className="size-5 text-emerald-300" />,
      title: "Connect what you already pay for",
      body: "Claude and Antigravity are subscriptions: they cost nothing at the margin. OpenRouter is metered, and it is the only key you need to paste. Setup checks each one and tells you what is missing.",
      action: "Open Setup",
      view: "setup" as const,
    },
    {
      icon: <LayoutDashboard className="size-5 text-sky-300" />,
      title: "Give Hermes a job, not a prompt",
      body: "A mission is a repo, a goal and acceptance criteria. Dispatch it and a worker does the work; a second agent reviews the diff; nothing merges without an approval, green CI and your allow switch.",
      action: "Open Missions",
      view: "mission" as const,
    },
    {
      icon: <Sparkles className="size-5 text-violet-300" />,
      title: "Let it learn",
      body: "Every finished mission records which model did the work, what it cost and whether it was approved. The router reads that, in shadow, and shows what it would pick before it is allowed to pick.",
      action: "Read the docs",
      view: "help" as const,
    },
  ];

  const s = steps[step];
  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/60 p-6">
      <div className="w-full max-w-lg rounded-2xl border border-line bg-panel p-6 shadow-2xl">
        <div className="mb-1 flex items-center gap-2 text-label text-slate-500">
          Step {step + 1} of {steps.length}
        </div>
        <div className="flex items-start gap-3">
          <div className="mt-0.5 grid size-10 shrink-0 place-items-center rounded-xl bg-white/[0.04] ring-1 ring-line">
            {s.icon}
          </div>
          <div>
            <h2 className="text-title font-semibold text-slate-100">{s.title}</h2>
            <p className="mt-1 text-body-sm leading-relaxed text-slate-400">{s.body}</p>
          </div>
        </div>

        <div className="mt-5 flex items-center gap-2">
          <button
            onClick={finish}
            className="inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-xs text-slate-400 hover:text-slate-200"
          >
            <Check className="size-3.5" /> Skip
          </button>
          <div className="flex-1" />
          {step > 0 && (
            <button
              onClick={() => setStep(step - 1)}
              className="inline-flex h-8 items-center rounded-lg px-3 text-xs text-slate-300 ring-1 ring-line hover:bg-white/[0.04]"
            >
              Back
            </button>
          )}
          <button
            onClick={() => {
              onGo(s.view);
              if (step === steps.length - 1) finish();
              else setStep(step + 1);
            }}
            className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-emerald-500/15 px-3 text-xs font-medium text-emerald-300 ring-1 ring-emerald-500/30 hover:bg-emerald-500/20"
          >
            {s.action} <ArrowRight className="size-3.5" />
          </button>
        </div>
      </div>
    </div>
  );
}
