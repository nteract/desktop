//! Shared numeric helpers for visualization summarization.

use serde_json::Value;

/// Basic descriptive statistics for a numeric series.
#[allow(dead_code)]
pub struct NumericStats {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub count: usize,
}

/// Extract f64 values from a JSON array, skipping nulls and non-numeric entries.
#[allow(dead_code)]
pub fn extract_numbers(arr: &[Value]) -> Vec<f64> {
    arr.iter()
        .filter_map(|v| v.as_f64())
        .filter(|v| v.is_finite())
        .collect()
}

/// Extract f64 values preserving position — returns `None` for nulls/non-numeric entries.
///
/// Use this instead of `extract_numbers` when you need to maintain index alignment
/// with a parallel array (e.g., labels and values).
pub fn extract_numbers_positional(arr: &[Value]) -> Vec<Option<f64>> {
    arr.iter()
        .map(|v| v.as_f64().filter(|f| f.is_finite()))
        .collect()
}

/// Compute basic descriptive statistics for a numeric slice.
pub fn compute_stats(values: &[f64]) -> Option<NumericStats> {
    if values.is_empty() {
        return None;
    }
    let count = values.len();
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    for &v in values {
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
        sum += v;
    }
    Some(NumericStats {
        min,
        max,
        mean: sum / count as f64,
        count,
    })
}

/// Detect the overall trend direction of a numeric series.
///
/// Uses a simple linear regression sign. Returns one of:
/// "increasing", "decreasing", "flat", or "mixed" (for very short/noisy data).
pub fn detect_trend(values: &[f64]) -> &'static str {
    if values.len() < 2 {
        return "flat";
    }

    let n = values.len() as f64;
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xy = 0.0;
    let mut sum_xx = 0.0;

    for (i, &y) in values.iter().enumerate() {
        let x = i as f64;
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_xx += x * x;
    }

    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < f64::EPSILON {
        return "flat";
    }

    let slope = (n * sum_xy - sum_x * sum_y) / denom;

    // Normalize slope relative to the mean to determine significance
    let mean = sum_y / n;
    if mean.abs() < f64::EPSILON {
        return "flat";
    }
    let relative_slope = slope / mean.abs();

    if relative_slope > 0.01 {
        "increasing"
    } else if relative_slope < -0.01 {
        "decreasing"
    } else {
        "flat"
    }
}

/// Format a float concisely for display.
///
/// - Whole numbers: no decimal point (e.g., `42`)
/// - Small numbers: up to 2 decimal places (e.g., `3.17`)
/// - Large numbers: scientific notation (e.g., `1.4e+04`)
pub fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let abs = v.abs();
    if abs == 0.0 {
        return "0".to_string();
    }
    // Use scientific notation for very large or very small values
    if abs >= 1e6 || (abs < 0.01 && abs > 0.0) {
        // Format with limited precision in scientific notation
        let s = format!("{:.2e}", v);
        return s;
    }
    // Check if it's effectively a whole number
    if (v - v.round()).abs() < 1e-9 {
        return format!("{}", v as i64);
    }
    // Otherwise, up to 2 decimal places, strip trailing zeros
    let s = format!("{:.2}", v);
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    s.to_string()
}

/// Extract a title from a JSON value that can be either a string or `{"text": "..."}`.
pub fn extract_title(v: &Value) -> Option<&str> {
    v.as_str().or_else(|| v.get("text")?.as_str())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::approx_constant)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_numbers() {
        let arr = vec![
            Value::from(1.0),
            Value::Null,
            Value::from(2.5),
            Value::from("not a number"),
            Value::from(3.0),
        ];
        assert_eq!(extract_numbers(&arr), vec![1.0, 2.5, 3.0]);
    }

    #[test]
    fn test_extract_numbers_empty() {
        assert_eq!(extract_numbers(&[]), Vec::<f64>::new());
    }

    #[test]
    fn test_compute_stats() {
        let stats = compute_stats(&[1.0, 2.0, 3.0, 4.0, 5.0]).expect("should have stats");
        assert!((stats.min - 1.0).abs() < f64::EPSILON);
        assert!((stats.max - 5.0).abs() < f64::EPSILON);
        assert!((stats.mean - 3.0).abs() < f64::EPSILON);
        assert_eq!(stats.count, 5);
    }

    #[test]
    fn test_compute_stats_empty() {
        assert!(compute_stats(&[]).is_none());
    }

    #[test]
    fn test_compute_stats_single() {
        let stats = compute_stats(&[42.0]).expect("should have stats");
        assert!((stats.min - 42.0).abs() < f64::EPSILON);
        assert!((stats.max - 42.0).abs() < f64::EPSILON);
        assert!((stats.mean - 42.0).abs() < f64::EPSILON);
        assert_eq!(stats.count, 1);
    }

    #[test]
    fn test_detect_trend_increasing() {
        assert_eq!(
            detect_trend(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
            "increasing"
        );
    }

    #[test]
    fn test_detect_trend_decreasing() {
        assert_eq!(
            detect_trend(&[8.0, 7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0]),
            "decreasing"
        );
    }

    #[test]
    fn test_detect_trend_flat() {
        assert_eq!(detect_trend(&[5.0, 5.0, 5.0, 5.0, 5.0]), "flat");
    }

    #[test]
    fn test_detect_trend_single() {
        assert_eq!(detect_trend(&[1.0]), "flat");
    }

    #[test]
    fn test_fmt_num_integer() {
        assert_eq!(fmt_num(42.0), "42");
    }

    #[test]
    fn test_fmt_num_decimal() {
        assert_eq!(fmt_num(3.17), "3.17");
    }

    #[test]
    fn test_fmt_num_trailing_zero() {
        assert_eq!(fmt_num(2.50), "2.5");
    }

    #[test]
    fn test_fmt_num_large() {
        assert_eq!(fmt_num(14000.0), "14000");
    }

    #[test]
    fn test_fmt_num_very_large() {
        assert_eq!(fmt_num(1400000.0), "1.40e6");
    }

    #[test]
    fn test_fmt_num_zero() {
        assert_eq!(fmt_num(0.0), "0");
    }

    #[test]
    fn test_fmt_num_negative() {
        assert_eq!(fmt_num(-3.17), "-3.17");
    }

    #[test]
    fn test_extract_title_string() {
        let v = Value::from("My Title");
        assert_eq!(extract_title(&v), Some("My Title"));
    }

    #[test]
    fn test_extract_title_object() {
        let v = serde_json::json!({"text": "My Title", "font": {"size": 14}});
        assert_eq!(extract_title(&v), Some("My Title"));
    }

    #[test]
    fn test_extract_title_missing() {
        let v = serde_json::json!(42);
        assert_eq!(extract_title(&v), None);
    }
}
