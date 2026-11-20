import { useCallback, useEffect, useRef, useState } from "react";
import { errorText } from "./api";

export type RecordTarget = { id: string; nonce: number };

export function useRecordTarget<T>(target: RecordTarget | null, read: (id: string) => Promise<T | null>) {
  const [result, setResult] = useState<{ target: RecordTarget; record: T | null; error: string | null } | null>(null);
  const [revision, setRevision] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const focused = useRef<RecordTarget | null>(null);
  const refresh = useCallback(() => setRevision((value) => value + 1), []);
  useEffect(() => {
    if (!target) return;
    let current = true;
    void read(target.id).then(
      (record) => { if (current) setResult({ target, record, error: null }); },
      (error) => { if (current) setResult({ target, record: null, error: errorText(error) }); },
    );
    return () => { current = false; };
  }, [target, read, revision]);
  const ready = result?.target === target;
  useEffect(() => {
    if (target && ready && focused.current !== target && ref.current) {
      focused.current = target;
      ref.current.focus({ preventScroll: true });
      ref.current.scrollIntoView({ block: "nearest" });
    }
  }, [target, ready]);
  return { record: ready ? result?.record ?? null : null, error: ready ? result?.error ?? null : null, loading: !!target && !ready, ref, refresh };
}

export type NavTarget = "setup" | "chat" | "mission" | "control" | "help" | "fleet" | "timeline";

export function goTo(view: NavTarget) {
  window.dispatchEvent(new CustomEvent<NavTarget>("agentos:go", { detail: view }));
}
