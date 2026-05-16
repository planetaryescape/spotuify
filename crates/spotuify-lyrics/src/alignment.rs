pub use spotuify_core::active_lyric_line_index as active_line_index;

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_core::LyricLine;

    fn line(start_ms: u64) -> LyricLine {
        LyricLine {
            start_ms,
            text: start_ms.to_string(),
            is_rtl: false,
        }
    }

    #[test]
    fn active_line_uses_latest_line_at_or_before_position() {
        let lines = vec![line(1_000), line(2_000), line(5_000)];
        assert_eq!(active_line_index(&lines, 2_500, 0), Some(1));
    }

    #[test]
    fn offset_adjusts_active_line() {
        let lines = vec![line(1_000), line(2_000), line(5_000)];
        assert_eq!(active_line_index(&lines, 1_500, 700), Some(1));
        assert_eq!(active_line_index(&lines, 2_500, -700), Some(0));
    }
}
