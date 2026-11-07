//! Block-character sparklines, single or multi row.
use super::text::{Seg, merge};
use crate::ui::explorer::paint::theme;
use bmux_tui::style::Style;

const BLOCKS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// Fill levels (0..=8 eighths) per cell, top row first. The newest value is
/// rightmost; missing history leaves the left side empty.
pub fn levels(values: &[f64], width: usize, height: usize, max: f64) -> Vec<Vec<u8>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let skip = values.len().saturating_sub(width);
    let shown = &values[skip..];
    let pad = width - shown.len();
    let steps = height.saturating_mul(8);
    let max = if max.is_finite() && max > 0.0 {
        max
    } else {
        1.0
    };
    let heights: Vec<usize> = std::iter::repeat_n(0, pad)
        .chain(shown.iter().map(|&v| {
            if !v.is_finite() || v <= 0.0 {
                return 0;
            }
            let level = ((v / max).min(1.0) * steps as f64).round() as usize;
            level.clamp(1, steps)
        }))
        .collect();
    (0..height)
        .map(|r| {
            let base = (height - 1 - r) * 8;
            heights
                .iter()
                .map(|&h| h.saturating_sub(base).min(8) as u8)
                .collect()
        })
        .collect()
}

/// Styled rows for a sparkline. Empty cells on the bottom row show a dim
/// baseline so a flat series still reads as a graph.
pub fn rows(values: &[f64], width: usize, height: usize, max: f64, style: Style) -> Vec<Vec<Seg>> {
    let grid = levels(values, width, height, max);
    let last = grid.len().saturating_sub(1);
    grid.into_iter()
        .enumerate()
        .map(|(r, row)| {
            merge(
                row.into_iter()
                    .map(|level| {
                        if level == 0 && r == last {
                            Seg::new("▁", theme::dim())
                        } else {
                            Seg::new(BLOCKS[level as usize], style)
                        }
                    })
                    .collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_scale_and_right_align() {
        let grid = levels(&[0.0, 50.0, 100.0], 5, 1, 100.0);
        assert_eq!(grid, vec![vec![0, 0, 0, 4, 8]]);
        let tall = levels(&[100.0, 25.0], 2, 2, 100.0);
        assert_eq!(tall, vec![vec![8, 0], vec![8, 4]]);
    }

    #[test]
    fn tiny_values_stay_visible_and_overflow_clamps() {
        assert_eq!(levels(&[0.001, 500.0], 2, 1, 100.0), vec![vec![1, 8]]);
        assert_eq!(levels(&[f64::NAN, -1.0], 2, 1, 0.0), vec![vec![0, 0]]);
        assert!(levels(&[1.0], 0, 3, 1.0).is_empty());
        assert!(levels(&[1.0], 3, 0, 1.0).is_empty());
    }

    #[test]
    fn only_the_newest_values_fit() {
        let values: Vec<f64> = (0..10).map(f64::from).collect();
        assert_eq!(levels(&values, 2, 1, 9.0), vec![vec![7, 8]]);
    }

    #[test]
    fn rows_draw_a_baseline() {
        let rows = rows(&[0.0, 1.0], 2, 2, 1.0, theme::accent());
        let text: Vec<String> = rows
            .iter()
            .map(|r| r.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(text, vec![" █".to_string(), "▁█".to_string()]);
    }
}
