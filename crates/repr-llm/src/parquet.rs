//! Format a `nteract_predicate::ParquetSummary` as compact text for LLM consumption.

use nteract_predicate::{ColumnStats, ColumnSummary, ParquetSummary};

/// Summarize a parquet dataset for LLM consumption.
/// Returns a compact multi-line string describing row count, size, and per-column stats.
pub fn summarize(summary: &ParquetSummary) -> String {
    let mut out = String::new();

    // Header
    let size = format_bytes(summary.num_bytes);
    if size.is_empty() {
        out.push_str(&format!(
            "Parquet dataset ({} rows × {} columns)\n",
            format_number(summary.num_rows),
            summary.columns.len()
        ));
    } else {
        out.push_str(&format!(
            "Parquet dataset ({} rows × {} columns, {})\n",
            format_number(summary.num_rows),
            summary.columns.len(),
            size
        ));
    }

    if summary.columns.is_empty() {
        return out;
    }

    out.push_str("\nColumns:\n");
    for col in &summary.columns {
        out.push_str(&format_column(col, summary.num_rows));
    }

    out
}

fn format_column(col: &ColumnSummary, total_rows: u64) -> String {
    let null_info = if col.null_count == 0 {
        String::new()
    } else if total_rows > 0 {
        let pct = (col.null_count as f64 / total_rows as f64 * 100.0).round();
        format!(
            " · {} nulls ({}%)",
            format_number(col.null_count),
            pct as u64
        )
    } else {
        format!(" · {} nulls", format_number(col.null_count))
    };

    let stats = match &col.stats {
        ColumnStats::Numeric { min, max } => {
            if min.is_nan() {
                String::new()
            } else {
                format!(
                    " · range {} – {}",
                    format_number_f64(*min),
                    format_number_f64(*max)
                )
            }
        }
        ColumnStats::Boolean {
            true_count,
            false_count,
        } => {
            let total = true_count + false_count;
            if total == 0 {
                String::new()
            } else {
                let t_pct = (*true_count as f64 / total as f64 * 100.0).round() as u64;
                format!(
                    " · true {}% / false {}%",
                    t_pct,
                    100_u64.saturating_sub(t_pct)
                )
            }
        }
        ColumnStats::String {
            distinct_count,
            distinct_count_capped,
            top,
        } => {
            let prefix = if *distinct_count_capped { "≥" } else { "" };
            let mut s = format!(" · {}{} distinct", prefix, format_number(*distinct_count));
            if *distinct_count_capped {
                s.push_str(" (sampled)");
            }
            if !top.is_empty() {
                let top_str: Vec<String> = top
                    .iter()
                    .take(3)
                    .map(|(label, count)| {
                        format!("{:?} ({})", truncate(label, 32), format_number(*count))
                    })
                    .collect();
                s.push_str(", top: ");
                s.push_str(&top_str.join(", "));
            }
            s
        }
        ColumnStats::Temporal { min, max } => {
            if min.is_empty() {
                String::new()
            } else {
                format!(" · {} to {}", min, max)
            }
        }
        ColumnStats::Other => String::new(),
    };

    format!("  {} ({}){}{}\n", col.name, col.data_type, null_info, stats)
}

fn format_bytes(n: u64) -> String {
    if n == 0 {
        return String::new();
    }
    if n < 1024 {
        return format!("{} B", n);
    }
    let kb = n as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{:.1} kB", kb);
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{:.1} MB", mb);
    }
    format!("{:.2} GB", mb / 1024.0)
}

fn format_number(n: u64) -> String {
    // Thousands separator
    let s = n.to_string();
    let mut out = String::new();
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

fn format_number_f64(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format_number(n as u64)
    } else {
        // Keep 3 significant digits after the decimal for readability
        format!("{:.3}", n)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 1).collect();
        format!("{}…", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_empty() {
        let s = summarize(&ParquetSummary {
            num_rows: 0,
            num_bytes: 0,
            columns: vec![],
        });
        assert!(s.contains("0 rows × 0 columns"));
    }

    #[test]
    fn summarize_with_columns() {
        let summary = ParquetSummary {
            num_rows: 1_200,
            num_bytes: 4_500_000,
            columns: vec![
                ColumnSummary {
                    name: "id".to_string(),
                    data_type: "int64".to_string(),
                    null_count: 0,
                    stats: ColumnStats::Numeric {
                        min: 1.0,
                        max: 1200.0,
                    },
                },
                ColumnSummary {
                    name: "active".to_string(),
                    data_type: "bool".to_string(),
                    null_count: 0,
                    stats: ColumnStats::Boolean {
                        true_count: 900,
                        false_count: 300,
                    },
                },
                ColumnSummary {
                    name: "name".to_string(),
                    data_type: "string".to_string(),
                    null_count: 12,
                    stats: ColumnStats::String {
                        distinct_count: 500,
                        distinct_count_capped: false,
                        top: vec![("alice".to_string(), 200), ("bob".to_string(), 150)],
                    },
                },
            ],
        };
        let s = summarize(&summary);
        assert!(s.contains("1,200 rows × 3 columns"));
        assert!(s.contains("4.3 MB"));
        assert!(s.contains("id (int64)"));
        assert!(s.contains("range 1 – 1,200"));
        assert!(s.contains("true 75% / false 25%"));
        assert!(s.contains("12 nulls (1%)"));
        assert!(s.contains("500 distinct"));
        assert!(s.contains("\"alice\""));
    }

    #[test]
    fn format_bytes_thresholds() {
        assert_eq!(format_bytes(0), "");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1500), "1.5 kB");
        assert_eq!(format_bytes(5_000_000), "4.8 MB");
    }

    #[test]
    fn format_bytes_gb_scale() {
        // Large parquet datasets cross the GB threshold in real usage.
        // Precision bumps to 2 decimals at that scale.
        assert_eq!(format_bytes(2_500_000_000), "2.33 GB");
    }

    #[test]
    fn format_number_adds_thousands_separators() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1_000), "1,000");
        assert_eq!(format_number(1_234_567), "1,234,567");
        assert_eq!(format_number(1_000_000_000), "1,000,000,000");
    }

    #[test]
    fn format_number_f64_keeps_integers_clean_and_rounds_floats() {
        // Integer-valued floats render as integers with separators, so row
        // counts or column indices don't leak ".0" into LLM output.
        assert_eq!(format_number_f64(0.0), "0");
        assert_eq!(format_number_f64(1_200.0), "1,200");
        // Fractional values keep 3-decimal precision.
        assert_eq!(format_number_f64(1.23456), "1.235");
        // Very large floats that exceed the integer cast window fall back
        // to fixed precision instead of overflowing.
        assert_eq!(format_number_f64(1e16), "10000000000000000.000");
    }

    #[test]
    fn truncate_respects_char_boundary_with_ellipsis() {
        // 32-char cap includes the ellipsis character. Multi-byte chars
        // (emoji, CJK) must not be cut mid-codepoint.
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 10), "abcdefghij");
        let out = truncate("abcdefghijklmnopqrstuvwxyz", 10);
        assert_eq!(out, "abcdefghi…");
        // Emoji stress: 4-byte chars at the cut point.
        let emoji = "🚀".repeat(20);
        let out = truncate(&emoji, 5);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn summarize_formats_numeric_nan_as_no_stats() {
        // NaN min means the column had no valid numeric values (e.g. all
        // nulls). Rendering "range NaN – NaN" would be useless noise.
        let summary = ParquetSummary {
            num_rows: 100,
            num_bytes: 0,
            columns: vec![ColumnSummary {
                name: "score".to_string(),
                data_type: "float64".to_string(),
                null_count: 100,
                stats: ColumnStats::Numeric {
                    min: f64::NAN,
                    max: f64::NAN,
                },
            }],
        };
        let s = summarize(&summary);
        assert!(s.contains("score (float64)"));
        assert!(s.contains("100 nulls (100%)"));
        assert!(!s.contains("range"));
        assert!(!s.contains("NaN"));
    }

    #[test]
    fn summarize_handles_capped_distinct_count() {
        // Big cardinality columns report `distinct_count_capped = true` —
        // the sampled flag must be surfaced so the LLM treats the number
        // as a lower bound.
        let summary = ParquetSummary {
            num_rows: 1_000_000,
            num_bytes: 0,
            columns: vec![ColumnSummary {
                name: "uuid".to_string(),
                data_type: "string".to_string(),
                null_count: 0,
                stats: ColumnStats::String {
                    distinct_count: 100_000,
                    distinct_count_capped: true,
                    top: vec![],
                },
            }],
        };
        let s = summarize(&summary);
        assert!(s.contains("≥100,000 distinct"));
        assert!(s.contains("(sampled)"));
    }

    #[test]
    fn summarize_renders_temporal_range() {
        let summary = ParquetSummary {
            num_rows: 500,
            num_bytes: 0,
            columns: vec![ColumnSummary {
                name: "ts".to_string(),
                data_type: "timestamp[us]".to_string(),
                null_count: 0,
                stats: ColumnStats::Temporal {
                    min: "2024-01-01T00:00:00".to_string(),
                    max: "2024-12-31T23:59:59".to_string(),
                },
            }],
        };
        let s = summarize(&summary);
        assert!(s.contains("2024-01-01T00:00:00 to 2024-12-31T23:59:59"));
    }

    #[test]
    fn summarize_temporal_empty_min_means_no_stats() {
        // Empty min string signals "no stats available" (e.g. ZSTD
        // decoded metadata was absent). Do not render " to " with empty
        // endpoints.
        let summary = ParquetSummary {
            num_rows: 1,
            num_bytes: 0,
            columns: vec![ColumnSummary {
                name: "ts".to_string(),
                data_type: "timestamp[us]".to_string(),
                null_count: 0,
                stats: ColumnStats::Temporal {
                    min: String::new(),
                    max: String::new(),
                },
            }],
        };
        let s = summarize(&summary);
        assert!(s.contains("ts (timestamp[us])"));
        assert!(!s.contains(" to "));
    }

    #[test]
    fn summarize_other_stats_render_just_the_header() {
        // Columns with unsupported stats (binary, struct, list) fall back
        // to `ColumnStats::Other`. Rendering must still include the
        // column name + type so the LLM knows the column exists.
        let summary = ParquetSummary {
            num_rows: 10,
            num_bytes: 0,
            columns: vec![ColumnSummary {
                name: "payload".to_string(),
                data_type: "binary".to_string(),
                null_count: 0,
                stats: ColumnStats::Other,
            }],
        };
        let s = summarize(&summary);
        assert!(s.contains("payload (binary)"));
    }

    #[test]
    fn summarize_null_count_without_total_rows() {
        // A degenerate zero-row summary can still carry null counts
        // (columns exist, just no rows). Avoid dividing by zero;
        // fall back to raw null count without a percent.
        let summary = ParquetSummary {
            num_rows: 0,
            num_bytes: 0,
            columns: vec![ColumnSummary {
                name: "x".to_string(),
                data_type: "int64".to_string(),
                null_count: 5,
                stats: ColumnStats::Other,
            }],
        };
        let s = summarize(&summary);
        assert!(s.contains("5 nulls"));
        assert!(!s.contains('%'));
    }

    #[test]
    fn summarize_all_false_boolean_renders_zero_percent_true() {
        // Edge case for the boolean percent math: with true_count = 0 and
        // false_count > 0, the "true X%" should be 0, not arithmetic error.
        let summary = ParquetSummary {
            num_rows: 100,
            num_bytes: 0,
            columns: vec![ColumnSummary {
                name: "flag".to_string(),
                data_type: "bool".to_string(),
                null_count: 0,
                stats: ColumnStats::Boolean {
                    true_count: 0,
                    false_count: 100,
                },
            }],
        };
        let s = summarize(&summary);
        assert!(s.contains("true 0% / false 100%"));
    }

    #[test]
    fn summarize_truncates_long_top_values() {
        // A very long top-K label (common for URLs, UUIDs embedded in
        // string columns) must be truncated so the LLM output stays
        // compact.
        let long_label = "x".repeat(100);
        let summary = ParquetSummary {
            num_rows: 50,
            num_bytes: 0,
            columns: vec![ColumnSummary {
                name: "url".to_string(),
                data_type: "string".to_string(),
                null_count: 0,
                stats: ColumnStats::String {
                    distinct_count: 2,
                    distinct_count_capped: false,
                    top: vec![(long_label, 30)],
                },
            }],
        };
        let s = summarize(&summary);
        assert!(s.contains('…'));
    }
}
