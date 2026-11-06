import { Check, Copy, FileDown } from "lucide-react";
import { memo, useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";
import { api, errorText } from "../lib/api";
import { useToast } from "./Toasts";
import { Button, Field, Modal, inputClass } from "./ui";

interface HastNode {
  type: string;
  value?: string;
  tagName?: string;
  properties?: { className?: string[] | string };
  children?: HastNode[];
  data?: { meta?: string };
}

function hastText(node: HastNode | undefined): string {
  if (!node) return "";
  if (node.type === "text") return node.value ?? "";
  return (node.children ?? []).map(hastText).join("");
}

/** A file path hint from the fence meta (```ts src/app.ts) or a first-line path comment. */
function fileHint(meta: string | undefined, code: string): string {
  const fromMeta = (meta ?? "").split(/\s+/).find((w) => /[\w-]+\.[\w]+$/.test(w) && !w.includes("="));
  if (fromMeta) return fromMeta.replace(/^(title|file|filename):/, "");
  const first = code.split("\n", 1)[0] ?? "";
  const m = first.match(/^\s*(?:\/\/|#|--|<!--)\s*(?:file|path)?:?\s*([\w./\\-]+\.[\w]+)\s*(?:-->)?\s*$/i);
  return m ? m[1] : "";
}

function CodeBlock({ node, children, workingDir }: { node?: HastNode; children: ReactNode; workingDir: string | null }) {
  const toast = useToast();
  const [copied, setCopied] = useState(false);
  const [applying, setApplying] = useState(false);
  const codeNode = node?.children?.find((c) => c.tagName === "code");
  const code = hastText(codeNode).replace(/\n$/, "");
  const cls = codeNode?.properties?.className;
  const lang = (Array.isArray(cls) ? cls : String(cls ?? "").split(" ")).find((c) => c.startsWith("language-"))?.slice(9) ?? "";
  const [path, setPath] = useState(() => fileHint(codeNode?.data?.meta, code));
  const [busy, setBusy] = useState(false);

  async function copy() {
    await navigator.clipboard.writeText(code);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
  }

  async function apply() {
    if (!workingDir) return;
    setBusy(true);
    try {
      const r = await api.applyCode(workingDir, path, code);
      toast("ok", `${r.created ? "Created" : "Updated"} ${r.path} (${r.bytes} bytes)`);
      setApplying(false);
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="group/code my-3 overflow-hidden rounded-xl border border-line bg-[#0a0e15]">
      <div className="flex items-center justify-between border-b border-line bg-white/[0.02] px-3 py-1.5">
        <span className="font-mono text-[11px] text-slate-500">{lang || "text"}</span>
        <div className="flex items-center gap-1">
          {workingDir && (
            <button onClick={() => setApplying(true)} className="inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-[11px] text-slate-400 hover:bg-white/5 hover:text-slate-100">
              <FileDown className="size-3.5" /> Apply
            </button>
          )}
          <button onClick={() => void copy()} className="inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-[11px] text-slate-400 hover:bg-white/5 hover:text-slate-100">
            {copied ? <Check className="size-3.5 text-emerald-400" /> : <Copy className="size-3.5" />}
            {copied ? "Copied" : "Copy"}
          </button>
        </div>
      </div>
      <pre className="overflow-x-auto p-3.5 font-mono text-[12.5px] leading-relaxed">{children}</pre>
      {applying && workingDir && (
        <Modal
          title="Apply code to a file"
          onClose={() => setApplying(false)}
          footer={
            <>
              <Button variant="ghost" onClick={() => setApplying(false)}>
                Cancel
              </Button>
              <Button variant="primary" busy={busy} disabled={!path.trim()} onClick={() => void apply()} icon={<FileDown className="size-4" />}>
                Write file
              </Button>
            </>
          }
        >
          <Field label="File path" hint={`Relative to ${workingDir}. Existing files are overwritten; line endings are kept.`}>
            <input autoFocus className={inputClass} value={path} onChange={(e) => setPath(e.target.value)} placeholder="src/main.rs" />
          </Field>
          <div className="mt-3 text-[11px] text-muted">{code.split("\n").length} lines</div>
        </Modal>
      )}
    </div>
  );
}

export const Markdown = memo(function Markdown({ text, workingDir }: { text: string; workingDir: string | null }) {
  return (
    <div className="prose-studio">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[[rehypeHighlight, { detect: true, ignoreMissing: true }]]}
        components={{
          pre: ({ node, children }) => (
            <CodeBlock node={node as unknown as HastNode} workingDir={workingDir}>
              {children}
            </CodeBlock>
          ),
          a: ({ href, children }) => (
            <a href={href} target="_blank" rel="noreferrer" className="text-emerald-300 underline decoration-emerald-500/40 underline-offset-2 hover:text-emerald-200">
              {children}
            </a>
          ),
          table: ({ children }) => (
            <div className="my-3 overflow-x-auto rounded-lg border border-line">
              <table>{children}</table>
            </div>
          ),
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
