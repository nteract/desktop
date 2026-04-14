from dx._refs import BLOB_REF_MIME, BlobRef, build_ref_bundle


def test_blob_ref_dataclass_fields():
    ref = BlobRef(hash="sha256:abc", size=42)
    assert ref.hash == "sha256:abc"
    assert ref.size == 42


def test_blob_ref_has_no_url_field():
    # URLs are session-ephemeral and never part of the persistent protocol.
    import dataclasses

    fields = {f.name for f in dataclasses.fields(BlobRef)}
    assert "url" not in fields


def test_ref_mime_constant():
    assert BLOB_REF_MIME == "application/vnd.nteract.blob-ref+json"


def test_build_ref_bundle_minimal():
    ref = BlobRef(hash="sha256:abc", size=10)
    bundle = build_ref_bundle(ref, content_type="image/png")
    assert bundle == {
        "hash": "sha256:abc",
        "content_type": "image/png",
        "size": 10,
        "query": None,
    }


def test_build_ref_bundle_with_summary():
    ref = BlobRef(hash="sha256:abc", size=10)
    summary = {
        "total_rows": 100,
        "included_rows": 50,
        "sampled": True,
        "sample_strategy": "head",
    }
    bundle = build_ref_bundle(ref, content_type="application/vnd.apache.parquet", summary=summary)
    assert bundle["summary"] == summary
    assert bundle["query"] is None


def test_build_ref_bundle_no_url_leak():
    ref = BlobRef(hash="sha256:abc", size=10)
    bundle = build_ref_bundle(ref, content_type="image/png")
    assert "url" not in bundle


def test_build_ref_bundle_query_reserved_null():
    ref = BlobRef(hash="sha256:abc", size=10)
    bundle = build_ref_bundle(ref, content_type="image/png")
    assert bundle["query"] is None
