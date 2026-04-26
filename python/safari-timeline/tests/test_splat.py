import base64
import json

from safari_timeline import SplatOptions, splat_recording

PNG_1X1 = base64.b64encode(
    b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01"
    b"\x08\x06\x00\x00\x00\x1f\x15\xc4\x89"
).decode("ascii")


def test_splat_recording_streams_records_and_extracts_screenshots(tmp_path):
    recording = tmp_path / "recording.json"
    recording.write_text(
        json.dumps(
            {
                "version": 1,
                "recording": {
                    "displayName": "Test Recording",
                    "startTime": 1.0,
                    "endTime": 2.0,
                    "instrumentTypes": [
                        "timeline-record-type-rendering-frame",
                        "timeline-record-type-screenshots",
                    ],
                    "records": [
                        {
                            "type": "timeline-record-type-rendering-frame",
                            "startTime": 1.0,
                            "endTime": 1.02,
                        },
                        {
                            "type": "timeline-record-type-screenshots",
                            "timestamp": 1.01,
                            "imageData": f"data:image/png;base64,{PNG_1X1}",
                        },
                        {
                            "type": "timeline-record-type-layout",
                            "startTime": 1.015,
                            "endTime": 1.016,
                            "data": {"text": 'brace } and quote " inside a string'},
                        },
                    ],
                },
            },
            separators=(",", ":"),
        ),
        encoding="utf-8",
    )

    output = tmp_path / "out"
    summary = splat_recording(recording, output, SplatOptions(screenshots="all"))

    assert summary["metadata"]["displayName"] == "Test Recording"
    assert summary["recordCounts"]["timeline-record-type-rendering-frame"] == 1
    assert summary["renderingFrames"]["maxMs"] == 20.000000000000018
    assert summary["screenshots"]["count"] == 1

    screenshot_files = list((output / "screenshots").glob("*.png"))
    assert len(screenshot_files) == 1
    assert screenshot_files[0].read_bytes().startswith(b"\x89PNG")

    lines = (output / "records" / "all.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(lines) == 3
    screenshot_record = json.loads(lines[1])
    assert "imageData" not in screenshot_record
    assert screenshot_record["imageFile"].startswith("screenshots/screenshot-00001")


def test_splat_recording_can_strip_screenshots(tmp_path):
    recording = tmp_path / "recording.json"
    recording.write_text(
        (
            '{"version":1,"recording":{"displayName":"No Images","startTime":0,'
            '"endTime":1,"instrumentTypes":[],"records":['
            '{"type":"timeline-record-type-screenshots","timestamp":0.5,'
            f'"imageData":"data:image/png;base64,{PNG_1X1}"}}'
            "]}}"
        ),
        encoding="utf-8",
    )

    output = tmp_path / "out"
    summary = splat_recording(recording, output, SplatOptions(screenshots="none"))

    assert summary["screenshots"]["count"] == 0
    assert not (output / "screenshots").exists()
    record = json.loads((output / "records" / "all.jsonl").read_text(encoding="utf-8"))
    assert record["imageDataRemoved"] is True
