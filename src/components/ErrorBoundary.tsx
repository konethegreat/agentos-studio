import { Component, type ErrorInfo, type ReactNode } from "react";

interface State {
  error: Error | null;
  info: string;
}

/** Shows render failures instead of an empty window, with a one-click recovery. */
export class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null, info: "" };

  static getDerivedStateFromError(error: Error): State {
    return { error, info: "" };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    this.setState({ error, info: info.componentStack ?? "" });
    console.error("AgentOS Studio render error", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="flex h-full items-center justify-center p-10">
        <div className="max-w-3xl rounded-2xl border border-red-500/30 bg-card p-6">
          <h1 className="mb-2 text-base font-semibold text-red-300">AgentOS Studio hit a rendering error</h1>
          <pre className="mb-4 max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-canvas p-3 font-mono text-xs text-slate-300">
            {this.state.error.message}
            {"\n"}
            {this.state.error.stack}
            {this.state.info}
          </pre>
          <button
            onClick={() => this.setState({ error: null, info: "" })}
            className="rounded-lg bg-emerald-500 px-4 py-2 text-sm font-semibold text-emerald-950 hover:bg-emerald-400"
          >
            Recover
          </button>
        </div>
      </div>
    );
  }
}
