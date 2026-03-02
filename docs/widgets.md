# Widget Support

This guide covers ipywidgets and anywidget support in nteract Desktop.

## Quick Reference

### Supported

| Category | Examples | Status |
|----------|----------|--------|
| **ipywidgets core** | IntSlider, Button, VBox, Dropdown | ✅ 49 widget types |
| **anywidget** | quak, drawdata, tqdm | ✅ Full AFM support |
| **ipycanvas** | Canvas, MultiCanvas | ✅ Custom implementation (tested with 0.14.3) |
| **Display outputs** | Plotly, Vega-Lite, HTML, images | ✅ Via display |

### Unsupported

| Widget | Why | Alternative |
|--------|-----|-------------|
| JupyterLab extensions | nteract Desktop is not JupyterLab | — |
| jupyterlab-sidecar | JupyterLab extension | Use notebook outputs |
| bqplot | Extends IPython's DOMWidget | Plotly, Altair, Vega-Lite |

## What Works

### Built-in ipywidgets

All standard `@jupyter-widgets/controls` widgets are implemented:

**Sliders & Progress**
- IntSlider, FloatSlider, FloatLogSlider
- IntRangeSlider, FloatRangeSlider
- SelectionSlider, SelectionRangeSlider
- IntProgress, FloatProgress

**Inputs**
- IntText, FloatText, BoundedIntText, BoundedFloatText
- Text, Textarea, Password
- Checkbox, ToggleButton
- ColorPicker, DatePicker, TimePicker, Datetime
- TagsInput, ColorsInput, IntsInput, FloatsInput
- FileUpload

**Selection**
- Dropdown, Select, SelectMultiple
- RadioButtons, ToggleButtons, Combobox

**Containers**
- VBox, HBox, Box, GridBox
- Accordion, Tab, Stack

**Display & Media**
- HTML, HTMLMath, Label
- Image, Audio, Video
- Button, Valid

**Other**
- Play (animation control)
- Controller (Gamepad API)
- Link, DirectionalLink (property sync)
- Output (nested outputs)

### ipycanvas

ipycanvas has a custom implementation (tested with v0.14.3). This is a from-scratch implementation of the canvas widget protocol, not using ipycanvas's original frontend code. It may be brittle with future ipycanvas versions.

### anywidget

nteract Desktop fully implements the [AFM (AnyWidget Frontend Module) spec](https://anywidget.dev/en/afm/). Any widget following this spec will work.

**Tested widgets:**
- **quak** — DataFrame viewer (custom messages work)
- **drawdata** — Drawing tool (dark mode works)
- **tqdm** — Progress bars (`leave=False` cleanup works)

**Supported features:**
- ESM loading (inline code and remote URLs)
- CSS injection
- Custom messages (`model.send()`)
- Binary buffers

### Standard Outputs

Rich display outputs work via the display protocol (not as widgets):
- Plotly, Vega-Lite, Vega
- HTML, Markdown, LaTeX
- Images (PNG, JPEG, SVG, GIF)
- JSON, GeoJSON

## What Doesn't Work

### JupyterLab Extensions

**nteract Desktop is NOT JupyterLab.** Anything requiring `@jupyterlab/*` APIs won't work:

- `jupyterlab-sidecar` — Creates JupyterLab panels, requires `@jupyterlab/application`
- Any widget that imports from `@jupyterlab/services`, `@jupyterlab/apputils`, etc.

### IPython DOMWidget Extensions

Some widgets extend IPython's `DOMWidget` class instead of the standard `@jupyter-widgets/base`. These use different internal APIs we don't implement.

**Known incompatible:**
- `bqplot` — Uses IPython DOMWidget internals
- `ipyleaflet` — Uses IPython DOMWidget internals

## Why These Limitations?

nteract Desktop runs widgets in isolated iframes for security. The architecture is:

```
Parent Window (Tauri app)
├── WidgetStore (manages state)
├── CommBridgeManager (routes messages)
└── PostMessage ↔ Iframe

Isolated Iframe (blob: URL, sandboxed)
├── Widget rendering
└── No access to Tauri APIs
```

This means:
1. We implement widget rendering from scratch, not via JupyterLab
2. JupyterLab-specific APIs don't exist
3. Custom widget classes need explicit support

## Recommendations

### For Charts and Plots

Use display outputs instead of widget-based libraries:

```python
import plotly.express as px
fig = px.scatter(df, x="x", y="y")
fig.show()  # Works via display output
```

Alternatives to bqplot: Plotly, Altair, Vega-Lite, Matplotlib

### For Custom Interactive Widgets

Use **anywidget**:

```python
import anywidget
import traitlets

class Counter(anywidget.AnyWidget):
    _esm = """
    export default {
      render({ model, el }) {
        const btn = document.createElement("button");
        btn.innerHTML = `Count: ${model.get("count")}`;
        btn.onclick = () => model.set("count", model.get("count") + 1);
        model.on("change:count", () => {
          btn.innerHTML = `Count: ${model.get("count")}`;
        });
        el.appendChild(btn);
      }
    }
    """
    count = traitlets.Int(0).tag(sync=True)
```

anywidget is the modern, portable approach that works across Jupyter environments.

### For Side Panels

Instead of `jupyterlab-sidecar`, use regular notebook outputs. Output appears inline in cells.

## See Also

- [GitHub Issue #44](https://github.com/runtimed/runt/issues/44) — Widget compatibility testing matrix
- [anywidget documentation](https://anywidget.dev/)
- `src/components/widgets/controls/` — Built-in widget implementations
