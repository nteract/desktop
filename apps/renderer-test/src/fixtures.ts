export interface Fixture {
  label: string;
  mimeType: string;
  data: unknown;
}

export const fixtures: Fixture[] = [
  {
    label: "Plain text",
    mimeType: "text/plain",
    data: "Hello from the renderer test app.\nThis is a second line.",
  },
  {
    label: "HTML",
    mimeType: "text/html",
    data: '<h2 style="color: steelblue;">HTML Output</h2><p>Rendered inside an isolated iframe.</p>',
  },
  {
    label: "JSON",
    mimeType: "application/json",
    data: JSON.stringify(
      { name: "renderer-test", version: "1.0.0", features: ["iframe", "plugins", "security"] },
      null,
      2,
    ),
  },
  {
    label: "SVG",
    mimeType: "image/svg+xml",
    data: '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100"><rect width="200" height="100" rx="10" fill="#4f46e5"/><text x="100" y="55" text-anchor="middle" fill="white" font-family="system-ui" font-size="16">SVG Output</text></svg>',
  },
  {
    label: "Markdown (plugin)",
    mimeType: "text/markdown",
    data: "# Markdown Plugin\n\nThis is rendered by the **markdown renderer plugin**.\n\n- Item 1\n- Item 2\n- Item 3\n\n```python\nprint('hello')\n```\n",
  },
  {
    label: "Plotly (plugin)",
    mimeType: "application/vnd.plotly.v1+json",
    data: JSON.stringify({
      data: [
        {
          x: [1, 2, 3, 4, 5],
          y: [2, 6, 3, 8, 5],
          type: "scatter",
          mode: "lines+markers",
          name: "Test Series",
        },
      ],
      layout: {
        title: "Plotly Plugin Test",
        width: 500,
        height: 300,
      },
    }),
  },
];
