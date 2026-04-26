from __future__ import annotations

import argparse
import base64
import json
import re
import statistics
from collections import Counter
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO, Literal

ScreenshotMode = Literal["all", "none"]

RECORDS_MARKER = b'"records":['
DATA_URL_PREFIX = "data:image/"


@dataclass(frozen=True)
class SplatOptions:
    screenshots: ScreenshotMode = "all"


def splat_recording(input_path: Path, output_dir: Path, options: SplatOptions) -> dict[str, Any]:
    output_dir.mkdir(parents=True, exist_ok=True)
    records_dir = output_dir / "records"
    screenshots_dir = output_dir / "screenshots"
    records_dir.mkdir(exist_ok=True)
    if options.screenshots == "all":
        screenshots_dir.mkdir(exist_ok=True)

    all_records = (records_dir / "all.jsonl").open("w", encoding="utf-8")
    per_type_handles: dict[str, Any] = {}
    screenshot_index = None
    if options.screenshots == "all":
        screenshot_index = (screenshots_dir / "index.jsonl").open("w", encoding="utf-8")

    metadata, record_iter = _open_record_stream(input_path)
    record_counts: Counter[str] = Counter()
    frame_durations: list[float] = []
    top_frames: list[dict[str, Any]] = []
    screenshot_count = 0

    try:
        for record_index, raw_record in enumerate(record_iter, start=1):
            record = json.loads(raw_record)
            record_type = str(record.get("type", "unknown"))
            record_counts[record_type] += 1

            if record_type == "timeline-record-type-rendering-frame":
                duration_ms = _duration_ms(record)
                if duration_ms is not None:
                    frame_durations.append(duration_ms)
                    _track_top_frame(top_frames, record_index, record, duration_ms)

            if isinstance(record.get("imageData"), str):
                image_data = record.pop("imageData")
                if options.screenshots == "all" and screenshot_index is not None:
                    screenshot_count += 1
                    image_name, image_size = _write_screenshot(
                        screenshots_dir, screenshot_count, record, image_data
                    )
                    record["imageFile"] = f"screenshots/{image_name}"
                    record["imageBytes"] = image_size
                    screenshot_index.write(
                        json.dumps(
                            {
                                "recordIndex": record_index,
                                "timestamp": record.get("timestamp"),
                                "file": image_name,
                                "bytes": image_size,
                            },
                            sort_keys=True,
                        )
                        + "\n"
                    )
                else:
                    record["imageDataRemoved"] = True

            line = json.dumps(record, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
            all_records.write(line + "\n")
            handle = _record_type_handle(records_dir, per_type_handles, record_type)
            handle.write(line + "\n")
    finally:
        all_records.close()
        if screenshot_index is not None:
            screenshot_index.close()
        for handle in per_type_handles.values():
            handle.close()

    summary = {
        "input": str(input_path),
        "metadata": metadata,
        "recordCounts": dict(sorted(record_counts.items())),
        "screenshots": {
            "mode": options.screenshots,
            "count": screenshot_count,
        },
        "renderingFrames": _frame_summary(frame_durations, top_frames),
    }
    (output_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return summary


def _open_record_stream(input_path: Path) -> tuple[dict[str, Any], Iterator[bytes]]:
    handle = input_path.open("rb")
    prefix, remainder = _read_until_records(handle)
    metadata = _parse_metadata(prefix)

    def records() -> Iterator[bytes]:
        try:
            yield from _iter_record_objects(handle, remainder)
        finally:
            handle.close()

    return metadata, records()


def _read_until_records(handle: BinaryIO) -> tuple[bytes, bytes]:
    chunks: list[bytes] = []
    while True:
        chunk = handle.read(1024 * 1024)
        if not chunk:
            raise ValueError("Could not find recording.records array")
        chunks.append(chunk)
        data = b"".join(chunks)
        marker_index = data.find(RECORDS_MARKER)
        if marker_index != -1:
            after_marker = marker_index + len(RECORDS_MARKER)
            return data[:after_marker], data[after_marker:]


def _parse_metadata(prefix: bytes) -> dict[str, Any]:
    text = prefix.decode("utf-8", errors="replace")
    metadata: dict[str, Any] = {}

    version = _regex_value(text, r'"version":(\d+)')
    if version is not None:
        metadata["version"] = int(version)

    display_name = _regex_value(text, r'"displayName":"((?:\\.|[^"])*)"')
    if display_name is not None:
        metadata["displayName"] = json.loads(f'"{display_name}"')

    for key in ("startTime", "endTime"):
        value = _regex_value(text, rf'"{key}":([0-9.]+)')
        if value is not None:
            metadata[key] = float(value)

    instruments = _regex_value(text, r'"instrumentTypes":(\[[^\]]*\])')
    if instruments is not None:
        metadata["instrumentTypes"] = json.loads(instruments)

    if "startTime" in metadata and "endTime" in metadata:
        metadata["durationSeconds"] = metadata["endTime"] - metadata["startTime"]

    return metadata


def _regex_value(text: str, pattern: str) -> str | None:
    match = re.search(pattern, text)
    return match.group(1) if match else None


def _iter_record_objects(handle: BinaryIO, initial: bytes) -> Iterator[bytes]:
    buffer = initial
    depth = 0
    in_string = False
    escaped = False
    record = bytearray()

    while True:
        if not buffer:
            buffer = handle.read(1024 * 1024)
            if not buffer:
                break

        for byte in buffer:
            if depth == 0:
                if byte == ord("{"):
                    depth = 1
                    in_string = False
                    escaped = False
                    record = bytearray(b"{")
                elif byte == ord("]"):
                    return
                continue

            record.append(byte)
            if in_string:
                if escaped:
                    escaped = False
                elif byte == ord("\\"):
                    escaped = True
                elif byte == ord('"'):
                    in_string = False
            elif byte == ord('"'):
                in_string = True
            elif byte == ord("{"):
                depth += 1
            elif byte == ord("}"):
                depth -= 1
                if depth == 0:
                    yield bytes(record)

        buffer = b""


def _duration_ms(record: dict[str, Any]) -> float | None:
    start = record.get("startTime")
    end = record.get("endTime")
    if isinstance(start, int | float) and isinstance(end, int | float):
        return (end - start) * 1000
    return None


def _track_top_frame(
    top_frames: list[dict[str, Any]], record_index: int, record: dict[str, Any], duration_ms: float
) -> None:
    top_frames.append(
        {
            "recordIndex": record_index,
            "startTime": record.get("startTime"),
            "endTime": record.get("endTime"),
            "durationMs": duration_ms,
        }
    )
    top_frames.sort(key=lambda frame: frame["durationMs"], reverse=True)
    del top_frames[20:]


def _write_screenshot(
    screenshots_dir: Path, screenshot_count: int, record: dict[str, Any], image_data: str
) -> tuple[str, int]:
    match = re.match(r"data:image/([a-zA-Z0-9.+-]+);base64,(.*)", image_data)
    if not match:
        raise ValueError("Unsupported screenshot imageData URL")

    extension = "jpg" if match.group(1).lower() == "jpeg" else match.group(1).lower()
    name = f"screenshot-{screenshot_count:05d}"
    timestamp = record.get("timestamp")
    if isinstance(timestamp, int | float):
        name += f"-{timestamp:.3f}"
    file_name = f"{name}.{extension}"
    image_bytes = base64.b64decode(match.group(2), validate=True)
    (screenshots_dir / file_name).write_bytes(image_bytes)
    return file_name, len(image_bytes)


def _record_type_handle(records_dir: Path, handles: dict[str, Any], record_type: str):
    safe_name = _safe_file_stem(record_type)
    if safe_name not in handles:
        handles[safe_name] = (records_dir / f"{safe_name}.jsonl").open("w", encoding="utf-8")
    return handles[safe_name]


def _safe_file_stem(value: str) -> str:
    safe = re.sub(r"[^A-Za-z0-9._-]+", "-", value).strip("-")
    return safe or "unknown"


def _frame_summary(
    frame_durations: list[float], top_frames: list[dict[str, Any]]
) -> dict[str, Any]:
    if not frame_durations:
        return {"count": 0, "topFrames": []}

    sorted_durations = sorted(frame_durations)
    return {
        "count": len(frame_durations),
        "minMs": min(frame_durations),
        "medianMs": statistics.median(frame_durations),
        "meanMs": statistics.fmean(frame_durations),
        "p90Ms": _quantile(sorted_durations, 0.90),
        "p95Ms": _quantile(sorted_durations, 0.95),
        "p99Ms": _quantile(sorted_durations, 0.99),
        "maxMs": max(frame_durations),
        "over16_7Ms": sum(duration > 16.667 for duration in frame_durations),
        "over33_3Ms": sum(duration > 33.333 for duration in frame_durations),
        "topFrames": top_frames,
    }


def _quantile(sorted_values: list[float], p: float) -> float:
    index = round((len(sorted_values) - 1) * p)
    return sorted_values[index]


def _build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Splat a Safari Web Inspector timeline recording into readable files."
    )
    parser.add_argument("input", type=Path, help="Safari timeline export JSON")
    parser.add_argument("output_dir", type=Path, help="Directory to write splat output")
    parser.add_argument(
        "--screenshots",
        choices=("all", "none"),
        default="all",
        help="Extract screenshot imageData to PNG files or strip it from JSONL output.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _build_arg_parser().parse_args(argv)
    summary = splat_recording(
        args.input,
        args.output_dir,
        SplatOptions(screenshots=args.screenshots),
    )
    frames = summary["renderingFrames"]
    print(f"Wrote Safari timeline splat to {args.output_dir}")
    print(f"Records: {sum(summary['recordCounts'].values())}")
    print(f"Screenshots: {summary['screenshots']['count']} ({summary['screenshots']['mode']})")
    if frames["count"]:
        print(
            "Frames: "
            f"{frames['count']} total, median {frames['medianMs']:.2f}ms, "
            f"p99 {frames['p99Ms']:.2f}ms, max {frames['maxMs']:.2f}ms"
        )
    return 0
