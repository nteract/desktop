export interface FrameHtmlOptions {
  /**
   * Whether to include dark mode styles by default.
   */
  darkMode?: boolean;
  /**
   * Additional CSS to inject into the frame.
   */
  additionalCss?: string;
  /**
   * Additional JavaScript to inject (runs after bootstrap).
   */
  additionalScript?: string;
}

/**
 * Generate the HTML template for an isolated output frame.
 *
 * The generated HTML includes:
 * - Basic styling for outputs (respects light/dark mode)
 * - Message handler for parent communication
 * - ResizeObserver for auto-sizing
 * - Ready notification on load
 *
 * @param options - Configuration options for the frame
 * @returns HTML string to be used with a blob URL
 */
export function generateFrameHtml(options: FrameHtmlOptions = {}): string {
  const {
    darkMode = true,
    additionalCss = "",
    additionalScript = "",
  } = options;

  return `<!DOCTYPE html>
<html>
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta http-equiv="Content-Security-Policy" content="default-src 'self' blob: data:; script-src 'unsafe-inline' 'unsafe-eval' blob: https:; style-src 'unsafe-inline'; img-src * data: blob:; font-src * data:; connect-src *;">
  <style>
    :root {
      --bg-primary: ${darkMode ? "#0a0a0a" : "#ffffff"};
      --bg-secondary: ${darkMode ? "#1a1a1a" : "#f5f5f5"};
      --text-primary: ${darkMode ? "#e0e0e0" : "#1a1a1a"};
      --text-secondary: ${darkMode ? "#a0a0a0" : "#666666"};
      --border-color: ${darkMode ? "#333333" : "#e0e0e0"};
      --accent-color: #3b82f6;
      --error-color: #ef4444;
      --success-color: #22c55e;
    }

    * {
      box-sizing: border-box;
    }

    html, body {
      margin: 0;
      padding: 0;
      font-family: system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
      font-size: 14px;
      line-height: 1.5;
      background: transparent;
      color: var(--text-primary);
    }

    body {
      padding: 8px;
    }

    /* Output container */
    #root {
      min-height: 1px;
    }

    /* Reset common elements - 0.875rem matches Tailwind's text-sm */
    pre, code {
      font-family: ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, monospace;
      font-size: 0.875rem;
    }

    pre {
      margin: 0;
      padding: 8px;
      background: var(--bg-secondary);
      border-radius: 4px;
      overflow-x: auto;
      white-space: pre-wrap;
      word-wrap: break-word;
    }

    /* Table styling for pandas DataFrames - 0.875rem matches Tailwind's text-sm */
    table {
      border-collapse: collapse;
      margin: 8px 0;
      font-size: 0.875rem;
    }

    th, td {
      border: 1px solid var(--border-color);
      padding: 4px 8px;
      text-align: left;
    }

    th {
      background: var(--bg-secondary);
      font-weight: 600;
    }

    /* Image outputs */
    img {
      max-width: 100%;
      height: auto;
    }

    /* Links */
    a {
      color: var(--accent-color);
      text-decoration: none;
    }

    a:hover {
      text-decoration: underline;
    }

    /* Error styling */
    .error {
      color: var(--error-color);
    }

    .error pre {
      background: ${darkMode ? "#1a1010" : "#fef2f2"};
      color: var(--error-color);
    }

    ${additionalCss}
  </style>
</head>
<body>
  <div id="root"></div>
  <script>
    (function() {
      'use strict';

      // --- State ---
      let isReady = false;
      const root = document.getElementById('root');

      // --- Message Handler ---
      // Note: When the React renderer bundle is loaded, it sets window.__REACT_RENDERER_ACTIVE__
      // and the inline handlers should defer to React for render/theme/clear messages.
      window.addEventListener('message', function(event) {
        // Only accept messages from our parent window
        if (event.source !== window.parent) {
          return;
        }

        const { type, payload } = event.data || {};

        try {
          switch (type) {
            case 'ping':
              handlePing(payload);
              break;

            case 'eval':
              handleEval(payload);
              break;

            case 'render':
              // Skip inline rendering if React renderer is active
              if (window.__REACT_RENDERER_ACTIVE__) return;
              handleRender(payload);
              break;

            case 'theme':
              // Skip inline theme handling if React renderer is active
              if (window.__REACT_RENDERER_ACTIVE__) return;
              handleTheme(payload);
              break;

            case 'clear':
              // Skip inline clear if React renderer is active
              if (window.__REACT_RENDERER_ACTIVE__) return;
              handleClear();
              break;

            case 'widget_state':
              handleWidgetState(payload);
              break;

            case 'search':
              handleSearch(payload);
              break;

            case 'search_navigate':
              handleSearchNavigate(payload);
              break;

            // Comm bridge messages - handled by React widget system, ignore here
            case 'bridge_ready':
            case 'comm_open':
            case 'comm_msg':
            case 'comm_close':
            case 'comm_sync':
              // These are handled by widget-bridge-client.ts
              break;

            default:
              console.warn('[frame] Unknown message type:', type);
          }
        } catch (err) {
          sendError(err);
        }
      });

      // --- Message Handlers ---

      function handlePing(payload) {
        send('pong', {
          receivedAt: Date.now(),
          echo: payload
        });
      }

      function handleEval(payload) {
        const { code } = payload || {};
        if (!code) {
          send('eval_result', { success: false, error: 'No code provided' });
          return;
        }

        // Store the current message for access during eval
        window.currentMessage = event;
        try {
          const result = eval.call(null, code);
          send('eval_result', { success: true, result: String(result ?? 'undefined') });
        } catch (err) {
          send('eval_result', { success: false, error: err.message });
        } finally {
          delete window.currentMessage;
        }
      }

      function handleRender(payload) {
        const { mimeType, data, metadata, append } = payload || {};

        // Create output container
        const output = document.createElement('div');
        output.className = 'output-item';
        output.style.marginBottom = '8px';

        if (mimeType === 'text/html') {
          // Use createContextualFragment for proper script execution
          const range = document.createRange();
          const fragment = range.createContextualFragment(String(data));
          output.appendChild(fragment);
        } else if (mimeType === 'text/plain') {
          const pre = document.createElement('pre');
          // Handle ANSI escape codes for colored output
          pre.innerHTML = parseAnsi(String(data));
          output.appendChild(pre);
        } else if (mimeType === 'image/svg+xml') {
          // SVG: render inline
          const container = document.createElement('div');
          container.innerHTML = String(data);
          const svg = container.querySelector('svg');
          if (svg) {
            svg.style.maxWidth = '100%';
            svg.style.height = 'auto';
            output.appendChild(svg);
          } else {
            output.appendChild(container);
          }
        } else if (mimeType && mimeType.startsWith('image/')) {
          const img = document.createElement('img');
          const imgData = String(data);
          // Check if it's base64 or a URL
          if (imgData.startsWith('data:') || imgData.startsWith('http')) {
            img.src = imgData;
          } else {
            img.src = 'data:' + mimeType + ';base64,' + imgData;
          }
          if (metadata?.width) img.width = metadata.width;
          if (metadata?.height) img.height = metadata.height;
          output.appendChild(img);
        } else if (mimeType === 'application/json') {
          // JSON: render as formatted, collapsible tree
          const pre = document.createElement('pre');
          try {
            const parsed = typeof data === 'string' ? JSON.parse(data) : data;
            pre.textContent = JSON.stringify(parsed, null, 2);
          } catch (e) {
            pre.textContent = String(data);
          }
          output.appendChild(pre);
        } else {
          // Fallback: render as text
          const pre = document.createElement('pre');
          pre.textContent = typeof data === 'string' ? data : JSON.stringify(data, null, 2);
          output.appendChild(pre);
        }

        // Append or replace
        if (append) {
          root.appendChild(output);
        } else {
          root.innerHTML = '';
          root.appendChild(output);
        }

        // Notify completion
        requestAnimationFrame(function() {
          send('render_complete', { height: document.body.scrollHeight });
        });
      }

      // Basic ANSI escape code parser
      function parseAnsi(text) {
        // Simple ANSI color mapping
        const colors = {
          '30': '#000', '31': '#e74c3c', '32': '#2ecc71', '33': '#f1c40f',
          '34': '#3498db', '35': '#9b59b6', '36': '#1abc9c', '37': '#ecf0f1',
          '90': '#7f8c8d', '91': '#e74c3c', '92': '#2ecc71', '93': '#f1c40f',
          '94': '#3498db', '95': '#9b59b6', '96': '#1abc9c', '97': '#fff'
        };

        // Escape HTML
        let result = text
          .replace(/&/g, '&amp;')
          .replace(/</g, '&lt;')
          .replace(/>/g, '&gt;');

        // Parse ANSI codes
        result = result.replace(/\\x1b\\[(\\d+(?:;\\d+)*)m/g, function(match, codes) {
          const codeList = codes.split(';');
          let style = '';
          for (const code of codeList) {
            if (code === '0') return '</span>';
            if (code === '1') style += 'font-weight:bold;';
            if (code === '3') style += 'font-style:italic;';
            if (code === '4') style += 'text-decoration:underline;';
            if (colors[code]) style += 'color:' + colors[code] + ';';
          }
          return style ? '<span style="' + style + '">' : '';
        });

        // Also handle \\e[ format
        result = result.replace(/\\e\\[(\\d+(?:;\\d+)*)m/g, function(match, codes) {
          const codeList = codes.split(';');
          let style = '';
          for (const code of codeList) {
            if (code === '0') return '</span>';
            if (code === '1') style += 'font-weight:bold;';
            if (colors[code]) style += 'color:' + colors[code] + ';';
          }
          return style ? '<span style="' + style + '">' : '';
        });

        return result;
      }

      function handleTheme(payload) {
        const { isDark, cssVariables } = payload || {};
        const rootEl = document.documentElement;

        if (isDark !== undefined) {
          rootEl.style.setProperty('--bg-primary', isDark ? '#0a0a0a' : '#ffffff');
          rootEl.style.setProperty('--bg-secondary', isDark ? '#1a1a1a' : '#f5f5f5');
          rootEl.style.setProperty('--text-primary', isDark ? '#e0e0e0' : '#1a1a1a');
          rootEl.style.setProperty('--text-secondary', isDark ? '#a0a0a0' : '#666666');
          rootEl.style.setProperty('--border-color', isDark ? '#333333' : '#e0e0e0');
        }

        if (cssVariables) {
          Object.entries(cssVariables).forEach(function([key, value]) {
            rootEl.style.setProperty(key, value);
          });
        }
      }

      function handleClear() {
        root.innerHTML = '';
        send('render_complete', { height: document.body.scrollHeight });
      }

      function handleWidgetState(payload) {
        // Widget state updates are handled by the injected renderer bundle
        // This is a placeholder that fires a custom event
        window.dispatchEvent(new CustomEvent('widget_state', { detail: payload }));
      }

      // --- Search ---
      var searchMarks = [];
      var currentSearchIndex = -1;

      function handleSearch(payload) {
        var query = (payload && payload.query) || '';
        var caseSensitive = payload && payload.caseSensitive;
        clearSearchMarks();
        if (!query) {
          send('search_results', { count: 0 });
          return;
        }
        var marks = [];
        var walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, null);
        var node;
        var compareQuery = caseSensitive ? query : query.toLowerCase();
        // Collect all text nodes and their match positions
        var matches = [];
        while ((node = walker.nextNode())) {
          var text = node.nodeValue || '';
          var compareText = caseSensitive ? text : text.toLowerCase();
          var pos = 0;
          while ((pos = compareText.indexOf(compareQuery, pos)) !== -1) {
            matches.push({ node: node, offset: pos, length: query.length });
            pos += query.length;
          }
        }
        // Highlight matches in reverse order to preserve offsets
        for (var i = matches.length - 1; i >= 0; i--) {
          var m = matches[i];
          try {
            var range = document.createRange();
            range.setStart(m.node, m.offset);
            range.setEnd(m.node, m.offset + m.length);
            var mark = document.createElement('mark');
            mark.className = 'global-find-match';
            mark.style.cssText = 'background: #fbbf24; color: #000; border-radius: 2px; padding: 0;';
            range.surroundContents(mark);
            marks.unshift(mark);
          } catch (e) {
            // surroundContents can fail if range crosses element boundaries
          }
        }
        searchMarks = marks;
        currentSearchIndex = -1;
        send('search_results', { count: marks.length });
      }

      function handleSearchNavigate(payload) {
        var matchIndex = (payload && payload.matchIndex) || 0;
        if (searchMarks.length === 0) return;
        // Clear previous active highlight
        if (currentSearchIndex >= 0 && currentSearchIndex < searchMarks.length) {
          searchMarks[currentSearchIndex].style.cssText = 'background: #fbbf24; color: #000; border-radius: 2px; padding: 0;';
        }
        currentSearchIndex = matchIndex;
        if (currentSearchIndex >= 0 && currentSearchIndex < searchMarks.length) {
          var active = searchMarks[currentSearchIndex];
          active.style.cssText = 'background: #f97316; color: #000; border-radius: 2px; padding: 0;';
          active.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
        }
      }

      function clearSearchMarks() {
        for (var i = 0; i < searchMarks.length; i++) {
          var mark = searchMarks[i];
          var parent = mark.parentNode;
          if (parent) {
            while (mark.firstChild) {
              parent.insertBefore(mark.firstChild, mark);
            }
            parent.removeChild(mark);
            parent.normalize();
          }
        }
        searchMarks = [];
        currentSearchIndex = -1;
      }

      // --- Utilities ---

      function send(type, payload) {
        window.parent.postMessage({ type: type, payload: payload }, '*');
      }

      function sendError(err) {
        send('error', {
          message: err.message || String(err),
          stack: err.stack
        });
      }

      // --- Resize Observer ---
      const resizeObserver = new ResizeObserver(function(entries) {
        const height = document.body.scrollHeight;
        send('resize', { height: height });
      });
      resizeObserver.observe(document.body);

      // --- Link Click Interception ---
      document.addEventListener('click', function(e) {
        const link = e.target.closest('a');
        if (link && link.href) {
          e.preventDefault();
          send('link_click', {
            url: link.href,
            newTab: e.metaKey || e.ctrlKey
          });
        }
      });

      // --- Double Click Forwarding ---
      document.addEventListener('dblclick', function(e) {
        // Don't forward double-clicks on links (user is selecting text)
        const link = e.target.closest('a');
        if (!link) {
          send('dblclick', null);
        }
      });

      // --- Error Handler ---
      window.addEventListener('error', function(e) {
        sendError(e.error || new Error(e.message));
      });

      window.addEventListener('unhandledrejection', function(e) {
        sendError(e.reason || new Error('Unhandled promise rejection'));
      });

      // --- Additional Script ---
      ${additionalScript}

      // --- Ready Signal ---
      isReady = true;
      send('ready', null);
    })();
  </script>
</body>
</html>`;
}

/**
 * Create a blob URL from the frame HTML.
 *
 * @param options - Configuration options for the frame
 * @returns A blob: URL that can be used as iframe src
 */
export function createFrameBlobUrl(options?: FrameHtmlOptions): string {
  const html = generateFrameHtml(options);
  const blob = new Blob([html], { type: "text/html" });
  return URL.createObjectURL(blob);
}
