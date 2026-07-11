use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;

use owo_colors::{OwoColorize, Style as OwoStyle};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const SEP: &str = "·";
pub const BULLET: &str = "•";
pub const CHECK: &str = "✓";
pub const ARROW: &str = "→";
pub const EMPTY: &str = "—";

static STDOUT_COLOR: OnceLock<bool> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
pub struct Style {
    color: bool,
}

impl Style {
    pub fn stdout() -> Self {
        Self {
            color: *STDOUT_COLOR.get_or_init(stdout_supports_color),
        }
    }

    #[cfg(test)]
    pub const fn plain() -> Self {
        Self { color: false }
    }

    fn paint(self, value: impl std::fmt::Display, style: OwoStyle) -> String {
        let value = value.to_string();
        if self.color {
            format!("{}", value.style(style))
        } else {
            value
        }
    }

    pub fn header(self, value: impl std::fmt::Display) -> String {
        self.paint(value, OwoStyle::new().bold().bright_black())
    }

    pub fn dim(self, value: impl std::fmt::Display) -> String {
        self.paint(value, OwoStyle::new().bright_black())
    }

    pub fn accent(self, value: impl std::fmt::Display) -> String {
        self.paint(value, OwoStyle::new().cyan())
    }

    pub fn success(self, value: impl std::fmt::Display) -> String {
        self.paint(value, OwoStyle::new().green())
    }

    pub fn warn(self, value: impl std::fmt::Display) -> String {
        self.paint(value, OwoStyle::new().yellow())
    }

    pub fn danger(self, value: impl std::fmt::Display) -> String {
        self.paint(value, OwoStyle::new().red())
    }

    pub fn count(self, value: impl std::fmt::Display) -> String {
        self.paint(value, OwoStyle::new().magenta())
    }
}

fn stdout_supports_color() -> bool {
    if std::env::var_os("CLICOLOR_FORCE").is_some_and(|value| value != "0") {
        return true;
    }
    std::env::var_os("NO_COLOR").is_none() && io::stdout().is_terminal()
}

#[derive(Clone, Copy, Debug)]
pub enum Alignment {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub struct Column {
    pub min: usize,
    pub max: usize,
    pub alignment: Alignment,
}

impl Column {
    pub const fn left(min: usize, max: usize) -> Self {
        Self {
            min,
            max,
            alignment: Alignment::Left,
        }
    }

    pub const fn right(min: usize, max: usize) -> Self {
        Self {
            min,
            max,
            alignment: Alignment::Right,
        }
    }
}

pub fn write_table<W: Write + ?Sized>(
    writer: &mut W,
    headers: &[&str],
    rows: &[Vec<String>],
    columns: &[Column],
    style: Style,
) -> io::Result<()> {
    // Non-TTY output intentionally keeps spacing/truncation while color gating
    // removes ANSI, so table output stays readable through grep and awk.
    debug_assert_eq!(headers.len(), columns.len());
    debug_assert!(rows.iter().all(|row| row.len() == columns.len()));

    let widths = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            rows.iter()
                .map(|row| UnicodeWidthStr::width(row[index].as_str()))
                .chain(std::iter::once(UnicodeWidthStr::width(headers[index])))
                .max()
                .unwrap_or(column.min)
                .clamp(column.min, column.max)
        })
        .collect::<Vec<_>>();

    if headers.iter().any(|header| !header.is_empty()) {
        write_row(writer, headers.iter().copied(), columns, &widths, |cell| {
            style.header(cell)
        })?;
    }
    for row in rows {
        write_row(
            writer,
            row.iter().map(String::as_str),
            columns,
            &widths,
            str::to_string,
        )?;
    }
    Ok(())
}

pub fn write_key_values<W, K, V>(
    writer: &mut W,
    rows: impl IntoIterator<Item = (K, V)>,
    style: Style,
) -> io::Result<()>
where
    W: Write + ?Sized,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let rows = rows
        .into_iter()
        .map(|(key, value)| vec![key.as_ref().to_string(), value.as_ref().to_string()])
        .collect::<Vec<_>>();
    let columns = [Column::left(1, 24), Column::left(1, 96)];
    let key_width = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(row[0].as_str()))
        .max()
        .unwrap_or(1)
        .clamp(columns[0].min, columns[0].max);
    for row in rows {
        let key = truncate(&row[0], key_width);
        let value = truncate(&row[1], columns[1].max);
        writeln!(
            writer,
            "{}  {}",
            style.dim(pad(&key, key_width, Alignment::Left)),
            value
        )?;
    }
    Ok(())
}

pub fn write_key_values_with_accent<W, K, V>(
    writer: &mut W,
    rows: impl IntoIterator<Item = (K, V, bool)>,
    style: Style,
) -> io::Result<()>
where
    W: Write + ?Sized,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let rows = rows
        .into_iter()
        .map(|(key, value, accent)| (key.as_ref().to_string(), value.as_ref().to_string(), accent))
        .collect::<Vec<_>>();
    let key_width = rows
        .iter()
        .map(|(key, _, _)| UnicodeWidthStr::width(key.as_str()))
        .max()
        .unwrap_or(1)
        .clamp(1, 24);
    for (key, value, accent) in rows {
        let key = pad(&truncate(&key, key_width), key_width, Alignment::Left);
        let value = truncate(&value, 96);
        let value = if accent { style.accent(value) } else { value };
        writeln!(writer, "{}  {}", style.dim(key), value)?;
    }
    Ok(())
}

fn write_row<'a, W: Write + ?Sized>(
    writer: &mut W,
    cells: impl Iterator<Item = &'a str>,
    columns: &[Column],
    widths: &[usize],
    decorate: impl Fn(&str) -> String,
) -> io::Result<()> {
    let cells = cells.collect::<Vec<_>>();
    for (index, cell) in cells.iter().enumerate() {
        let truncated = truncate(cell, widths[index]);
        if index + 1 == cells.len() {
            write!(writer, "{}", decorate(&truncated))?;
        } else {
            let padded = pad(&truncated, widths[index], columns[index].alignment);
            write!(writer, "{}", decorate(&padded))?;
        }
        if index + 1 < cells.len() {
            write!(writer, "  ")?;
        }
    }
    writeln!(writer)
}

fn pad(value: &str, width: usize, alignment: Alignment) -> String {
    let padding = width.saturating_sub(UnicodeWidthStr::width(value));
    match alignment {
        Alignment::Left => format!("{value}{}", " ".repeat(padding)),
        Alignment::Right => format!("{}{value}", " ".repeat(padding)),
    }
}

pub fn truncate(value: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let content_width = max_width - 1;
    let mut rendered = String::new();
    let mut width = 0;
    for ch in value.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + char_width > content_width {
            break;
        }
        rendered.push(ch);
        width += char_width;
    }
    rendered.push('…');
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_is_unicode_width_aware() {
        assert_eq!(truncate("ab界cd", 5), "ab界…");
        assert_eq!(UnicodeWidthStr::width(truncate("ab界cd", 5).as_str()), 5);
    }
}
