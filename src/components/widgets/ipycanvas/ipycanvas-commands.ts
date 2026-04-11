export const COMMANDS = [
  "fillRect",
  "strokeRect",
  "fillRects",
  "strokeRects",
  "clearRect",
  "fillArc",
  "fillCircle",
  "strokeArc",
  "strokeCircle",
  "fillArcs",
  "strokeArcs",
  "fillCircles",
  "strokeCircles",
  "strokeLine",
  "beginPath",
  "closePath",
  "stroke",
  "strokePath",
  "fillPath",
  "fill",
  "moveTo",
  "lineTo",
  "rect",
  "arc",
  "ellipse",
  "arcTo",
  "quadraticCurveTo",
  "bezierCurveTo",
  "fillText",
  "strokeText",
  "setLineDash",
  "drawImage",
  "putImageData",
  "clip",
  "save",
  "restore",
  "translate",
  "rotate",
  "scale",
  "transform",
  "setTransform",
  "resetTransform",
  "set",
  "clear",
  "sleep",
  "fillPolygon",
  "strokePolygon",
  "strokeLines",
  "fillPolygons",
  "strokePolygons",
  "strokeLineSegments",
  "fillStyledRects",
  "strokeStyledRects",
  "fillStyledCircles",
  "strokeStyledCircles",
  "fillStyledArcs",
  "strokeStyledArcs",
  "fillStyledPolygons",
  "strokeStyledPolygons",
  "strokeStyledLineSegments",
  "switchCanvas",
] as const;

/**
 * Canvas 2D context attribute names, indexed by their protocol number.
 * Used by the "set" command to update context properties.
 */
const CANVAS_ATTRS = [
  "fillStyle",
  "strokeStyle",
  "globalAlpha",
  "font",
  "textAlign",
  "textBaseline",
  "direction",
  "globalCompositeOperation",
  "lineWidth",
  "lineCap",
  "lineJoin",
  "miterLimit",
  "lineDashOffset",
  "shadowOffsetX",
  "shadowOffsetY",
  "shadowBlur",
  "shadowColor",
  "filter",
  "imageSmoothingEnabled",
] as const;

// === Binary Buffer Helpers ===

type TypedArray =
  | Int8Array
  | Uint8Array
  | Int16Array
  | Uint16Array
  | Int32Array
  | Uint32Array
  | Float32Array
  | Float64Array;

/**
 * Convert a DataView to the appropriate TypedArray based on dtype metadata.
 */
export function getTypedArray(dataview: DataView, metadata: { dtype: string }): TypedArray {
  const buffer = dataview.buffer;
  switch (metadata.dtype) {
    case "int8":
      return new Int8Array(buffer);
    case "uint8":
      return new Uint8Array(buffer);
    case "int16":
      return new Int16Array(buffer);
    case "uint16":
      return new Uint16Array(buffer);
    case "int32":
      return new Int32Array(buffer);
    case "uint32":
      return new Uint32Array(buffer);
    case "float32":
      return new Float32Array(buffer);
    case "float64":
      return new Float64Array(buffer);
    default:
      throw new Error(`Unknown dtype: ${metadata.dtype}`);
  }
}

// === Argument Wrappers ===

/**
 * Abstract argument that can be indexed into.
 * Used for batch operations where arguments may be scalars or buffer-backed arrays.
 */
interface Arg {
  getItem(idx: number): number;
  length: number;
}

class ScalarArg implements Arg {
  constructor(public value: number | boolean | string | null) {
    this.length = Infinity;
  }

  getItem(_idx: number): number {
    return this.value as number;
  }

  length: number;
}

class BufferArg implements Arg {
  private data: TypedArray;

  constructor(bufferMetadata: { dtype: string }, buffer: DataView) {
    this.data = getTypedArray(buffer, bufferMetadata);
    this.length = this.data.length;
  }

  getItem(idx: number): number {
    return this.data[idx] as number;
  }

  length: number;
}

/**
 * Resolve a command argument to either a scalar or buffer-backed array.
 * Scalars repeat the same value for all indices.
 * Buffer args reference a specific binary buffer by index.
 */
function getArg(metadata: unknown, buffers: DataView[]): Arg {
  if (
    metadata === null ||
    typeof metadata === "boolean" ||
    typeof metadata === "number" ||
    typeof metadata === "string"
  ) {
    return new ScalarArg(metadata);
  }

  if (typeof metadata === "object" && metadata !== null && "idx" in metadata) {
    const meta = metadata as { idx: number; dtype: string };
    return new BufferArg(meta, buffers[meta.idx]);
  }

  throw new Error(`Could not process argument: ${JSON.stringify(metadata)}`);
}

// === Drawing Helpers ===

function drawRects(
  _ctx: CanvasRenderingContext2D,
  args: unknown[],
  buffers: DataView[],
  callback: (x: number, y: number, w: number, h: number) => void,
) {
  const x = getArg(args[0], buffers);
  const y = getArg(args[1], buffers);
  const width = getArg(args[2], buffers);
  const height = getArg(args[3], buffers);
  const count = Math.min(x.length, y.length, width.length, height.length);

  for (let i = 0; i < count; i++) {
    callback(x.getItem(i), y.getItem(i), width.getItem(i), height.getItem(i));
  }
}

function drawCircles(
  _ctx: CanvasRenderingContext2D,
  args: unknown[],
  buffers: DataView[],
  callback: (x: number, y: number, r: number) => void,
) {
  const x = getArg(args[0], buffers);
  const y = getArg(args[1], buffers);
  const radius = getArg(args[2], buffers);
  const count = Math.min(x.length, y.length, radius.length);

  for (let i = 0; i < count; i++) {
    callback(x.getItem(i), y.getItem(i), radius.getItem(i));
  }
}

function drawArcs(
  _ctx: CanvasRenderingContext2D,
  args: unknown[],
  buffers: DataView[],
  callback: (
    x: number,
    y: number,
    r: number,
    startAngle: number,
    endAngle: number,
    anticlockwise: boolean,
  ) => void,
) {
  const x = getArg(args[0], buffers);
  const y = getArg(args[1], buffers);
  const radius = getArg(args[2], buffers);
  const startAngle = getArg(args[3], buffers);
  const endAngle = getArg(args[4], buffers);
  const anticlockwise = getArg(args[5], buffers);
  const count = Math.min(x.length, y.length, radius.length, startAngle.length, endAngle.length);

  for (let i = 0; i < count; i++) {
    callback(
      x.getItem(i),
      y.getItem(i),
      radius.getItem(i),
      startAngle.getItem(i),
      endAngle.getItem(i),
      !!anticlockwise.getItem(i),
    );
  }
}

function fillArc(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  radius: number,
  startAngle: number,
  endAngle: number,
  anticlockwise: boolean,
) {
  ctx.beginPath();
  ctx.moveTo(x, y);
  ctx.lineTo(x + radius * Math.cos(startAngle), y + radius * Math.sin(startAngle));
  ctx.arc(x, y, radius, startAngle, endAngle, anticlockwise);
  ctx.lineTo(x, y);
  ctx.fill();
  ctx.closePath();
}

function strokeArc(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  radius: number,
  startAngle: number,
  endAngle: number,
  anticlockwise: boolean,
) {
  ctx.beginPath();
  ctx.arc(x, y, radius, startAngle, endAngle, anticlockwise);
  ctx.stroke();
  ctx.closePath();
}

function fillCircle(ctx: CanvasRenderingContext2D, x: number, y: number, radius: number) {
  ctx.beginPath();
  ctx.arc(x, y, radius, 0, 2 * Math.PI);
  ctx.fill();
  ctx.closePath();
}

function strokeCircle(ctx: CanvasRenderingContext2D, x: number, y: number, radius: number) {
  ctx.beginPath();
  ctx.arc(x, y, radius, 0, 2 * Math.PI);
  ctx.stroke();
  ctx.closePath();
}

function drawPolygonPoints(ctx: CanvasRenderingContext2D, args: unknown[], buffers: DataView[]) {
  const points = getArg(args[0], buffers);
  ctx.beginPath();
  ctx.moveTo(points.getItem(0), points.getItem(1));
  for (let i = 2; i < points.length; i += 2) {
    ctx.lineTo(points.getItem(i), points.getItem(i + 1));
  }
}

function drawPolygonOrLineSegments(
  ctx: CanvasRenderingContext2D,
  args: unknown[],
  buffers: DataView[],
  fill: boolean,
  close: boolean,
) {
  const numPolygons = args[0] as number;
  const points = getArg(args[1], buffers);
  const sizes = getArg(args[2], buffers);

  let start = 0;
  for (let idx = 0; idx < numPolygons; idx++) {
    const size = sizes.getItem(idx) * 2;
    const stop = start + size;

    ctx.beginPath();
    ctx.moveTo(points.getItem(start), points.getItem(start + 1));
    for (let idp = start + 2; idp < stop; idp += 2) {
      ctx.lineTo(points.getItem(idp), points.getItem(idp + 1));
    }
    start = stop;
    if (close) ctx.closePath();
    fill ? ctx.fill() : ctx.stroke();
  }
}

function setStyle(ctx: CanvasRenderingContext2D, style: string, fill: boolean) {
  if (fill) {
    ctx.fillStyle = style;
  } else {
    ctx.strokeStyle = style;
  }
}

function drawStyledRects(
  ctx: CanvasRenderingContext2D,
  args: unknown[],
  buffers: DataView[],
  fill: boolean,
) {
  const x = getArg(args[0], buffers);
  const y = getArg(args[1], buffers);
  const width = getArg(args[2], buffers);
  const height = getArg(args[3], buffers);
  const colors = getArg(args[4], buffers);
  const alpha = getArg(args[5], buffers);
  const count = Math.min(x.length, y.length, width.length, height.length);

  ctx.save();
  for (let i = 0; i < count; i++) {
    const ci = 3 * i;
    const color = `rgba(${colors.getItem(ci)}, ${colors.getItem(ci + 1)}, ${colors.getItem(ci + 2)}, ${alpha.getItem(i)})`;
    setStyle(ctx, color, fill);
    if (fill) {
      ctx.fillRect(x.getItem(i), y.getItem(i), width.getItem(i), height.getItem(i));
    } else {
      ctx.strokeRect(x.getItem(i), y.getItem(i), width.getItem(i), height.getItem(i));
    }
  }
  ctx.restore();
}

function drawStyledCircles(
  ctx: CanvasRenderingContext2D,
  args: unknown[],
  buffers: DataView[],
  fill: boolean,
) {
  const x = getArg(args[0], buffers);
  const y = getArg(args[1], buffers);
  const radius = getArg(args[2], buffers);
  const colors = getArg(args[3], buffers);
  const alpha = getArg(args[4], buffers);
  const count = Math.min(x.length, y.length, radius.length);

  ctx.save();
  for (let i = 0; i < count; i++) {
    const ci = 3 * i;
    const color = `rgba(${colors.getItem(ci)}, ${colors.getItem(ci + 1)}, ${colors.getItem(ci + 2)}, ${alpha.getItem(i)})`;
    setStyle(ctx, color, fill);
    if (fill) {
      fillCircle(ctx, x.getItem(i), y.getItem(i), radius.getItem(i));
    } else {
      strokeCircle(ctx, x.getItem(i), y.getItem(i), radius.getItem(i));
    }
  }
  ctx.restore();
}

function drawStyledArcs(
  ctx: CanvasRenderingContext2D,
  args: unknown[],
  buffers: DataView[],
  fill: boolean,
) {
  const x = getArg(args[0], buffers);
  const y = getArg(args[1], buffers);
  const radius = getArg(args[2], buffers);
  const startAngle = getArg(args[3], buffers);
  const endAngle = getArg(args[4], buffers);
  const anticlockwise = getArg(args[5], buffers);
  const colors = getArg(args[6], buffers);
  const alpha = getArg(args[7], buffers);
  const count = Math.min(x.length, y.length, radius.length, startAngle.length, endAngle.length);

  ctx.save();
  for (let i = 0; i < count; i++) {
    const ci = 3 * i;
    const color = `rgba(${colors.getItem(ci)}, ${colors.getItem(ci + 1)}, ${colors.getItem(ci + 2)}, ${alpha.getItem(i)})`;
    setStyle(ctx, color, fill);
    if (fill) {
      fillArc(
        ctx,
        x.getItem(i),
        y.getItem(i),
        radius.getItem(i),
        startAngle.getItem(i),
        endAngle.getItem(i),
        !!anticlockwise.getItem(i),
      );
    } else {
      strokeArc(
        ctx,
        x.getItem(i),
        y.getItem(i),
        radius.getItem(i),
        startAngle.getItem(i),
        endAngle.getItem(i),
        !!anticlockwise.getItem(i),
      );
    }
  }
  ctx.restore();
}

function drawStyledPolygonOrLineSegments(
  ctx: CanvasRenderingContext2D,
  args: unknown[],
  buffers: DataView[],
  fill: boolean,
  close: boolean,
) {
  const numPolygons = args[0] as number;
  const points = getArg(args[1], buffers);
  const sizes = getArg(args[2], buffers);
  const colors = getArg(args[3], buffers);
  const alpha = getArg(args[4], buffers);

  ctx.save();
  let start = 0;
  for (let idx = 0; idx < numPolygons; idx++) {
    const ci = 3 * idx;
    const color = `rgba(${colors.getItem(ci)}, ${colors.getItem(ci + 1)}, ${colors.getItem(ci + 2)}, ${alpha.getItem(idx)})`;
    setStyle(ctx, color, fill);

    const size = sizes.getItem(idx) * 2;
    const stop = start + size;

    ctx.beginPath();
    ctx.moveTo(points.getItem(start), points.getItem(start + 1));
    for (let idp = start + 2; idp < stop; idp += 2) {
      ctx.lineTo(points.getItem(idp), points.getItem(idp + 1));
    }
    start = stop;
    if (close) ctx.closePath();
    fill ? ctx.fill() : ctx.stroke();
  }
  ctx.restore();
}

// === Command Processor ===

export interface ProcessCommandsResult {
  /** Whether a sleep command was encountered (caller should schedule next frame) */
  sleepMs: number | null;
  /** The model ID of the canvas that switchCanvas last targeted (null = default/unchanged) */
  switchedTo: string | null;
}

/**
 * Process a batch of ipycanvas drawing commands on a canvas 2D context.
 *
 * @param ctx - The 2D rendering context to draw on
 * @param command - The parsed command(s) from the JSON buffer
 * @param buffers - Remaining binary buffers for batch operations
 * @param canvasEl - The canvas element (for clear operations)
 * @param myModelId - This canvas's model ID (for switchCanvas filtering)
 * @param isActive - Whether this canvas is the current draw target
 * @returns Processing result with sleep/switch info
 */
export async function processCommands(
  ctx: CanvasRenderingContext2D,
  command: unknown,
  buffers: DataView[],
  canvasEl: HTMLCanvasElement,
  myModelId: string,
  isActive: boolean = true,
): Promise<ProcessCommandsResult> {
  const result: ProcessCommandsResult = { sleepMs: null, switchedTo: null };

  // Handle nested command arrays (batched via hold_canvas)
  if (Array.isArray(command) && command.length > 0 && Array.isArray(command[0])) {
    let remainingBuffers = buffers;
    let active = isActive;

    for (const subcommand of command) {
      let subbuffers: DataView[] = [];
      const nBuffers = subcommand[2] as number | undefined;
      if (nBuffers) {
        subbuffers = remainingBuffers.slice(0, nBuffers);
        remainingBuffers = remainingBuffers.slice(nBuffers);
      }
      const sub = await processCommands(ctx, subcommand, subbuffers, canvasEl, myModelId, active);
      if (sub.switchedTo !== null) {
        active = sub.switchedTo === myModelId;
        result.switchedTo = sub.switchedTo;
      }
      if (sub.sleepMs !== null) {
        result.sleepMs = sub.sleepMs;
      }
    }
    return result;
  }

  // Single command: [commandIndex, args, nBuffers?]
  const cmd = command as [number, unknown[]?, number?];
  const name = COMMANDS[cmd[0]];
  const args: unknown[] = cmd[1] ?? [];

  // Handle switchCanvas
  if (name === "switchCanvas") {
    const targetRef = args[0] as string;
    // Extract model ID from IPY_MODEL_ reference
    const targetId =
      typeof targetRef === "string" && targetRef.startsWith("IPY_MODEL_")
        ? targetRef.slice(10)
        : targetRef;
    result.switchedTo = targetId;
    return result;
  }

  // Skip commands not targeted at this canvas
  if (!isActive) {
    return result;
  }

  switch (name) {
    // --- Simple drawing ---
    case "fillRect":
      ctx.fillRect(args[0] as number, args[1] as number, args[2] as number, args[3] as number);
      break;
    case "strokeRect":
      ctx.strokeRect(args[0] as number, args[1] as number, args[2] as number, args[3] as number);
      break;
    case "clearRect":
      ctx.clearRect(args[0] as number, args[1] as number, args[2] as number, args[3] as number);
      break;
    case "fillCircle":
      fillCircle(ctx, args[0] as number, args[1] as number, args[2] as number);
      break;
    case "strokeCircle":
      strokeCircle(ctx, args[0] as number, args[1] as number, args[2] as number);
      break;
    case "fillArc":
      fillArc(
        ctx,
        args[0] as number,
        args[1] as number,
        args[2] as number,
        args[3] as number,
        args[4] as number,
        args[5] as boolean,
      );
      break;
    case "strokeArc":
      strokeArc(
        ctx,
        args[0] as number,
        args[1] as number,
        args[2] as number,
        args[3] as number,
        args[4] as number,
        args[5] as boolean,
      );
      break;
    case "strokeLine": {
      ctx.beginPath();
      ctx.moveTo(args[0] as number, args[1] as number);
      ctx.lineTo(args[2] as number, args[3] as number);
      ctx.stroke();
      ctx.closePath();
      break;
    }

    // --- Batch drawing ---
    case "fillRects":
      drawRects(ctx, args, buffers, (x, y, w, h) => ctx.fillRect(x, y, w, h));
      break;
    case "strokeRects":
      drawRects(ctx, args, buffers, (x, y, w, h) => ctx.strokeRect(x, y, w, h));
      break;
    case "fillCircles":
      drawCircles(ctx, args, buffers, (x, y, r) => fillCircle(ctx, x, y, r));
      break;
    case "strokeCircles":
      drawCircles(ctx, args, buffers, (x, y, r) => strokeCircle(ctx, x, y, r));
      break;
    case "fillArcs":
      drawArcs(ctx, args, buffers, (x, y, r, sa, ea, ac) => fillArc(ctx, x, y, r, sa, ea, ac));
      break;
    case "strokeArcs":
      drawArcs(ctx, args, buffers, (x, y, r, sa, ea, ac) => strokeArc(ctx, x, y, r, sa, ea, ac));
      break;
    case "strokeLines": {
      const points = getArg(args[0], buffers);
      ctx.beginPath();
      ctx.moveTo(points.getItem(0), points.getItem(1));
      for (let i = 2; i < points.length; i += 2) {
        ctx.lineTo(points.getItem(i), points.getItem(i + 1));
      }
      ctx.stroke();
      ctx.closePath();
      break;
    }
    case "fillPolygon": {
      drawPolygonPoints(ctx, args, buffers);
      ctx.closePath();
      ctx.fill();
      break;
    }
    case "strokePolygon": {
      drawPolygonPoints(ctx, args, buffers);
      ctx.closePath();
      ctx.stroke();
      break;
    }
    case "fillPolygons":
      drawPolygonOrLineSegments(ctx, args, buffers, true, true);
      break;
    case "strokePolygons":
      drawPolygonOrLineSegments(ctx, args, buffers, false, true);
      break;
    case "strokeLineSegments":
      drawPolygonOrLineSegments(ctx, args, buffers, false, false);
      break;

    // --- Styled batch ---
    case "fillStyledRects":
      drawStyledRects(ctx, args, buffers, true);
      break;
    case "strokeStyledRects":
      drawStyledRects(ctx, args, buffers, false);
      break;
    case "fillStyledCircles":
      drawStyledCircles(ctx, args, buffers, true);
      break;
    case "strokeStyledCircles":
      drawStyledCircles(ctx, args, buffers, false);
      break;
    case "fillStyledArcs":
      drawStyledArcs(ctx, args, buffers, true);
      break;
    case "strokeStyledArcs":
      drawStyledArcs(ctx, args, buffers, false);
      break;
    case "fillStyledPolygons":
      drawStyledPolygonOrLineSegments(ctx, args, buffers, true, true);
      break;
    case "strokeStyledPolygons":
      drawStyledPolygonOrLineSegments(ctx, args, buffers, false, true);
      break;
    case "strokeStyledLineSegments":
      drawStyledPolygonOrLineSegments(ctx, args, buffers, false, false);
      break;

    // --- Path operations ---
    case "beginPath":
      ctx.beginPath();
      break;
    case "closePath":
      ctx.closePath();
      break;
    case "stroke":
      ctx.stroke();
      break;
    case "fill":
      ctx.fill();
      break;
    case "moveTo":
      ctx.moveTo(args[0] as number, args[1] as number);
      break;
    case "lineTo":
      ctx.lineTo(args[0] as number, args[1] as number);
      break;
    case "rect":
      ctx.rect(args[0] as number, args[1] as number, args[2] as number, args[3] as number);
      break;
    case "arc":
      ctx.arc(
        args[0] as number,
        args[1] as number,
        args[2] as number,
        args[3] as number,
        args[4] as number,
        args[5] as boolean,
      );
      break;
    case "ellipse":
      ctx.ellipse(
        args[0] as number,
        args[1] as number,
        args[2] as number,
        args[3] as number,
        args[4] as number,
        args[5] as number,
        args[6] as number,
        args[7] as boolean,
      );
      break;
    case "arcTo":
      ctx.arcTo(
        args[0] as number,
        args[1] as number,
        args[2] as number,
        args[3] as number,
        args[4] as number,
      );
      break;
    case "quadraticCurveTo":
      ctx.quadraticCurveTo(
        args[0] as number,
        args[1] as number,
        args[2] as number,
        args[3] as number,
      );
      break;
    case "bezierCurveTo":
      ctx.bezierCurveTo(
        args[0] as number,
        args[1] as number,
        args[2] as number,
        args[3] as number,
        args[4] as number,
        args[5] as number,
      );
      break;
    case "clip":
      ctx.clip();
      break;

    // --- Text ---
    case "fillText":
      ctx.fillText(args[0] as string, args[1] as number, args[2] as number);
      break;
    case "strokeText":
      ctx.strokeText(args[0] as string, args[1] as number, args[2] as number);
      break;

    // --- Transform ---
    case "translate":
      ctx.translate(args[0] as number, args[1] as number);
      break;
    case "rotate":
      ctx.rotate(args[0] as number);
      break;
    case "scale":
      ctx.scale(args[0] as number, args[1] as number);
      break;
    case "transform":
      ctx.transform(
        args[0] as number,
        args[1] as number,
        args[2] as number,
        args[3] as number,
        args[4] as number,
        args[5] as number,
      );
      break;
    case "setTransform":
      ctx.setTransform(
        args[0] as number,
        args[1] as number,
        args[2] as number,
        args[3] as number,
        args[4] as number,
        args[5] as number,
      );
      break;
    case "resetTransform":
      ctx.resetTransform();
      break;

    // --- State ---
    case "save":
      ctx.save();
      break;
    case "restore":
      ctx.restore();
      break;
    case "set": {
      const attrIndex = args[0] as number;
      const attrName = CANVAS_ATTRS[attrIndex];
      if (attrName) {
        (ctx as unknown as Record<string, unknown>)[attrName] = args[1];
      }
      break;
    }
    case "setLineDash":
      ctx.setLineDash(args[0] as number[]);
      break;

    // --- Utility ---
    case "clear":
      ctx.clearRect(0, 0, canvasEl.width, canvasEl.height);
      break;
    case "sleep":
      result.sleepMs = args[0] as number;
      await new Promise((resolve) => setTimeout(resolve, result.sleepMs!));
      break;

    // --- Deferred (require cross-widget model resolution) ---
    case "drawImage":
    case "putImageData":
    case "strokePath":
    case "fillPath":
      // These require resolving IPY_MODEL_ references to other widget models
      // which is not yet supported. Silently skip.
      break;

    default:
      // Fallback: try to call the method directly on the context
      if (name && typeof (ctx as unknown as Record<string, unknown>)[name] === "function") {
        (ctx as unknown as Record<string, (...a: unknown[]) => void>)[name](...args);
      }
      break;
  }

  return result;
}
