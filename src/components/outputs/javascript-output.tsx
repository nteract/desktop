import { useEffect, useRef } from "react";

interface JavaScriptOutputProps {
  /**
   * JavaScript code to execute
   */
  code: string;
  /**
   * Additional CSS classes
   */
  className?: string;
}

/**
 * Check if the current window is inside an iframe.
 */
function isInIframe(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.self !== window.top;
  } catch {
    return true;
  }
}

/**
 * Executes JavaScript code from notebook outputs.
 * Only runs inside an isolated iframe as a defense-in-depth measure.
 * The isolation model already routes this type to the iframe, but
 * this check prevents execution if the component is somehow rendered
 * in the main DOM.
 */
export function JavaScriptOutput({ code, className = "" }: JavaScriptOutputProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!code || !containerRef.current || !isInIframe()) return;

    // Wrap the user code so it receives `element` (JupyterLab convention)
    // and `el` (nteract convention) pointing to the output container.
    const wrapper = `(function(element, el) {\n${code}\n})(document.currentScript.parentElement, document.currentScript.parentElement);`;
    const script = document.createElement("script");
    script.textContent = wrapper;
    containerRef.current.appendChild(script);

    return () => {
      if (containerRef.current?.contains(script)) {
        containerRef.current.removeChild(script);
      }
    };
  }, [code]);

  if (!isInIframe()) {
    return (
      <div className="py-2 px-3 text-sm text-muted-foreground bg-muted/50 rounded border border-border">
        <span className="font-medium">JavaScript output</span>
        <span className="mx-1">&middot;</span>
        <span>Requires iframe isolation to execute</span>
      </div>
    );
  }

  return <div data-slot="javascript-output" className={className} ref={containerRef} />;
}
