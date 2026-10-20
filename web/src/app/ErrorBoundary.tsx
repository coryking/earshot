import { Component, type ReactNode } from "react";

/** A crash shows a plain way out instead of a blank page. */
export class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <main className="mx-auto max-w-md px-6 py-16">
        <h1 className="text-2xl font-semibold text-heading">Something on this page broke.</h1>
        <p className="mt-2 text-muted-foreground">The radio itself is fine. Reload to get back to it.</p>
        <button
          type="button"
          onClick={() => location.reload()}
          className="mt-6 h-11 rounded-lg bg-primary px-5 text-primary-foreground"
        >
          Reload
        </button>
        <details className="mt-6 text-sm text-muted-foreground">
          <summary>Details for Cory</summary>
          <pre className="mt-2 whitespace-pre-wrap">{this.state.error.message}</pre>
        </details>
      </main>
    );
  }
}
