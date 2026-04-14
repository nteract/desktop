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
            top,
        } => {
            let mut s = format!(" · {} distinct", format_number(*distinct_count));
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
}
