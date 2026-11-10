//! Chart geometry.
//!
//! Line charts are emitted as SVG path commands in a fixed view box
//! (`width = capacity - 1`, `height = 100`) so the UI can draw them with a
//! single `Path` element whose `viewbox-*` matches. Samples are right-aligned:
//! the newest sample always sits on the right edge and the chart scrolls left.

use std::fmt::Write;

pub const VIEW_HEIGHT: f32 = 100.0;

/// View-box width for a series of the given capacity.
pub fn view_width(capacity: usize) -> f32 {
    (capacity.max(2) - 1) as f32
}

fn points(values: &[f32], capacity: usize, max: f32) -> impl Iterator<Item = (f32, f32)> + '_ {
    let capacity = capacity.max(2);
    let n = values.len().min(capacity);
    let skip = values.len() - n;
    let x0 = (capacity - n) as f32;
    let max = if max > 0.0 && max.is_finite() { max } else { 1.0 };
    values[skip..].iter().enumerate().map(move |(i, v)| {
        let v = if v.is_finite() { *v } else { 0.0 };
        let y = VIEW_HEIGHT - (v / max).clamp(0.0, 1.0) * VIEW_HEIGHT;
        (x0 + i as f32, y)
    })
}

/// Polyline through the samples. Empty input yields a degenerate path at the
/// bottom-right so the `Path` element always has valid commands.
pub fn line(values: &[f32], capacity: usize, max: f32) -> String {
    let w = view_width(capacity);
    if values.is_empty() {
        return format!("M {w} {VIEW_HEIGHT} L {w} {VIEW_HEIGHT}");
    }
    let mut out = String::with_capacity(values.len() * 14);
    for (i, (x, y)) in points(values, capacity, max).enumerate() {
        let cmd = if i == 0 { 'M' } else { 'L' };
        let _ = write!(out, "{cmd} {x:.2} {y:.2} ");
    }
    if values.len() == 1 {
        // A single sample: draw a short horizontal tick.
        let (x, y) = points(values, capacity, max).next().unwrap();
        let _ = write!(out, "L {:.2} {y:.2}", (x - 1.0).max(0.0));
    }
    out.trim_end().to_string()
}

/// Closed area under the line, for the faint fill.
pub fn area(values: &[f32], capacity: usize, max: f32) -> String {
    let w = view_width(capacity);
    if values.len() < 2 {
        return format!("M {w} {VIEW_HEIGHT} L {w} {VIEW_HEIGHT} Z");
    }
    let n = values.len().min(capacity.max(2));
    let x0 = (capacity.max(2) - n) as f32;
    format!("{} L {w:.2} {VIEW_HEIGHT} L {x0:.2} {VIEW_HEIGHT} Z", line(values, capacity, max))
}

/// Last `columns` samples normalised to 0–1, left-padded with zeros; used by
/// the dot-matrix column charts.
pub fn columns(values: &[f32], columns: usize, max: f32) -> Vec<f32> {
    let max = if max > 0.0 && max.is_finite() { max } else { 1.0 };
    let n = values.len().min(columns);
    let mut out = vec![0.0; columns - n];
    out.extend(
        values[values.len() - n..].iter().map(
            |v| {
                if v.is_finite() { (v / max).clamp(0.0, 1.0) } else { 0.0 }
            },
        ),
    );
    out
}

/// Rounds `v` up to 1, 2 or 5 × 10ⁿ — stable axis maxima for rate charts.
pub fn nice_max(v: f32, floor: f32) -> f32 {
    let v = v.max(floor).max(f32::MIN_POSITIVE);
    let exp = v.log10().floor();
    let base = 10f32.powf(exp);
    let m = v / base;
    let nice = if m <= 1.0 {
        1.0
    } else if m <= 2.0 {
        2.0
    } else if m <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_is_right_aligned_and_scaled() {
        let p = line(&[0.0, 50.0, 100.0], 5, 100.0);
        assert_eq!(p, "M 2.00 100.00 L 3.00 50.00 L 4.00 0.00");
    }

    #[test]
    fn line_clamps_out_of_range_and_nan() {
        let p = line(&[-10.0, 200.0, f32::NAN], 3, 100.0);
        assert_eq!(p, "M 0.00 100.00 L 1.00 0.00 L 2.00 100.00");
    }

    #[test]
    fn line_keeps_only_capacity_samples() {
        let p = line(&[1.0, 2.0, 3.0, 4.0], 2, 4.0);
        assert_eq!(p, "M 0.00 25.00 L 1.00 0.00");
    }

    #[test]
    fn empty_and_single_inputs_are_valid_paths() {
        assert!(line(&[], 10, 1.0).starts_with('M'));
        assert_eq!(line(&[50.0], 3, 100.0), "M 2.00 50.00 L 1.00 50.00");
        assert!(area(&[], 10, 1.0).ends_with('Z'));
    }

    #[test]
    fn area_closes_to_baseline() {
        let a = area(&[100.0, 100.0], 3, 100.0);
        assert_eq!(a, "M 1.00 0.00 L 2.00 0.00 L 2.00 100 L 1.00 100 Z");
    }

    #[test]
    fn zero_max_does_not_divide_by_zero() {
        assert_eq!(line(&[0.0, 0.0], 2, 0.0), "M 0.00 100.00 L 1.00 100.00");
    }

    #[test]
    fn columns_pad_and_normalise() {
        assert_eq!(columns(&[50.0], 3, 100.0), vec![0.0, 0.0, 0.5]);
        assert_eq!(columns(&[10.0, 20.0, 30.0, 40.0], 2, 40.0), vec![0.75, 1.0]);
    }

    #[test]
    fn nice_max_rounds_up() {
        assert_eq!(nice_max(0.7, 0.0), 1.0);
        assert_eq!(nice_max(1.3, 0.0), 2.0);
        assert_eq!(nice_max(3.0, 0.0), 5.0);
        assert_eq!(nice_max(730.0, 0.0), 1000.0);
        assert_eq!(nice_max(10.0, 1024.0), 2000.0);
    }
}
