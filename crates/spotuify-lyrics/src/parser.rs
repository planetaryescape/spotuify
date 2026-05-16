use spotuify_core::LyricLine;
use unicode_bidi::BidiInfo;

pub fn parse_lrc(input: &str) -> Vec<LyricLine> {
    let input = input.trim_start_matches('\u{feff}');
    let mut parsed = Vec::new();
    let mut last_index = None;

    for raw_line in input.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let Some((timestamps, text)) = split_timestamps(line) else {
            continue;
        };
        let text = text.trim();
        if timestamps.is_empty() {
            if let Some(index) = last_index {
                append_continuation(&mut parsed[index], text);
            }
            continue;
        }
        for start_ms in timestamps {
            parsed.push(LyricLine {
                start_ms,
                text: text.to_string(),
                is_rtl: is_rtl(text),
            });
            last_index = Some(parsed.len() - 1);
        }
    }

    parsed.sort_by_key(|line| line.start_ms);
    parsed
}

pub fn plain_text_lines(text: &str) -> Vec<LyricLine> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, text)| LyricLine {
            start_ms: index as u64,
            text: text.to_string(),
            is_rtl: is_rtl(text),
        })
        .collect()
}

pub fn is_rtl(text: &str) -> bool {
    BidiInfo::new(text, None)
        .paragraphs
        .first()
        .map(|paragraph| paragraph.level.is_rtl())
        .unwrap_or(false)
}

fn split_timestamps(line: &str) -> Option<(Vec<u64>, &str)> {
    let mut rest = line;
    let mut timestamps = Vec::new();
    while let Some(after_open) = rest.strip_prefix('[') {
        let (tag, after_tag) = after_open.split_once(']')?;
        if let Some(timestamp) = parse_timestamp(tag) {
            timestamps.push(timestamp);
            rest = after_tag;
            continue;
        }
        if timestamps.is_empty() {
            return None;
        }
        break;
    }
    Some((timestamps, rest))
}

fn parse_timestamp(value: &str) -> Option<u64> {
    let (minutes, rest) = value.split_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let (seconds, fraction) = match rest.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (rest, None),
    };
    let seconds = seconds.parse::<u64>().ok()?;
    if seconds >= 60 {
        return None;
    }
    let millis = match fraction {
        Some(raw) if raw.len() == 1 => raw.parse::<u64>().ok()? * 100,
        Some(raw) if raw.len() == 2 => raw.parse::<u64>().ok()? * 10,
        Some(raw) if raw.len() == 3 => raw.parse::<u64>().ok()?,
        Some(_) => return None,
        None => 0,
    };
    Some(minutes * 60_000 + seconds * 1_000 + millis)
}

fn append_continuation(line: &mut LyricLine, text: &str) {
    if text.is_empty() {
        return;
    }
    if !line.text.is_empty() {
        line.text.push('\n');
    }
    line.text.push_str(text);
    line.is_rtl = is_rtl(&line.text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_and_three_digit_milliseconds() {
        let lines = parse_lrc("[00:01.23]first\n[02:03.456]second");
        assert_eq!(lines[0].start_ms, 1_230);
        assert_eq!(lines[0].text, "first");
        assert_eq!(lines[1].start_ms, 123_456);
    }

    #[test]
    fn duplicates_multiple_timestamps_on_one_line() {
        let lines = parse_lrc("[00:01.00][00:02.00]echo");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start_ms, 1_000);
        assert_eq!(lines[1].start_ms, 2_000);
        assert!(lines.iter().all(|line| line.text == "echo"));
    }

    #[test]
    fn appends_untimed_lines_to_previous_timestamp() {
        let lines = parse_lrc("[00:01.00]first\ncontinued");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "first\ncontinued");
    }

    #[test]
    fn skips_bom_and_malformed_timestamp_lines() {
        let lines = parse_lrc("\u{feff}[bad]skip\n[00:02.00]keep");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].start_ms, 2_000);
        assert_eq!(lines[0].text, "keep");
    }

    #[test]
    fn marks_rtl_lines() {
        let lines = parse_lrc("[00:01.00]שלום");
        assert!(lines[0].is_rtl);
    }
}
