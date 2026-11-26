//! Terminal rendering shared by every command: sections, key/value lines,
//! markers and tables — byte-faithful to the source CLI's `ui.ts` (read
//! 2026-11-24).
//!
//! Pure string builders: a command decides which lines a screen has, the
//! binary prints them. Column widths are counted in characters (the
//! source counts UTF-16 units); every cell these commands emit is ASCII, so
//! the two agree.

/// The section bar: `── Title ─────…`, a uniform 57-column line while the
/// title fits (`ui.section`, ui.ts L9-12). The source's leading newline is
/// the caller's `line()`: an empty string pushed before this one.
pub fn section(title: &str) -> String {
    let fill = (56usize.saturating_sub(title.len() + 3)).max(3);
    format!("\u{2500}\u{2500} {title} {}", "\u{2500}".repeat(fill))
}

/// `ui.kv` (ui.ts L34): two spaces, key padded to 22, one space, value.
pub fn kv(key: &str, value: &str) -> String {
    format!("  {key:<22} {value}")
}

/// `ui.info` (ui.ts L30): the dim bullet.
pub fn info(text: &str) -> String {
    format!("\u{b7} {text}")
}

/// `ui.warn` (ui.ts L26): the attention marker.
pub fn warn(text: &str) -> String {
    format!("! {text}")
}

/// `ui.success` (ui.ts L18): the pass marker.
pub fn success(text: &str) -> String {
    format!("\u{2714} {text}")
}

/// `ui.failure` (ui.ts L22): the fail marker.
pub fn failure(text: &str) -> String {
    format!("\u{2716} {text}")
}

/// A line the host composed, with the tone it chose. The host decides which
/// sentence is said and how it reads; this palette is the client's — the
/// same split `usage.report` makes with finished lines.
pub fn toned(tone: &str, text: &str) -> String {
    match tone {
        "info" => info(text),
        "success" => success(text),
        "warn" => warn(text),
        _ => text.to_owned(),
    }
}

/// One line the host composed (the wire shape of `lines` entries).
#[derive(Debug, serde::Deserialize)]
pub struct ScreenLine {
    pub tone: String,
    pub text: String,
}

/// Prints the host's lines through this client's glyph palette.
pub fn screen(lines: &[ScreenLine]) -> String {
    lines
        .iter()
        .map(|line| toned(&line.tone, &line.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `ui.table` (ui.ts L42-61): header, a rule of column widths, then rows —
/// two spaces between columns, columns sized to their widest cell. An empty
/// row set prints the source's `(nothing to show)` line.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    if rows.is_empty() {
        return vec!["  (nothing to show)".to_owned()];
    }
    let mut widths: Vec<usize> = headers.iter().map(|head| head.chars().count()).collect();
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            let width = cell.chars().count();
            if column < widths.len() && width > widths[column] {
                widths[column] = width;
            }
        }
    }
    let join = |cells: Vec<String>| {
        cells
            .iter()
            .enumerate()
            .map(|(column, cell)| {
                let width = widths.get(column).copied().unwrap_or(0);
                format!("{cell:<width$}")
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    let mut lines = vec![
        format!(
            "  {}",
            join(headers.iter().map(|head| (*head).to_owned()).collect())
        ),
        format!(
            "  {}",
            widths
                .iter()
                .map(|width| "\u{2500}".repeat(*width))
                .collect::<Vec<_>>()
                .join("  ")
        ),
    ];
    for row in rows {
        lines.push(format!("  {}", join(row.clone())));
    }
    lines
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_section_bar_is_a_uniform_width() {
        assert_eq!(
            section("Setup"),
            format!("\u{2500}\u{2500} Setup {}", "\u{2500}".repeat(48))
        );
        assert_eq!(section("LLM providers").chars().count(), 57);
        // A title too long for the 56-column body keeps the minimum fill of
        // three dashes instead of shrinking below it.
        let long = section(&"t".repeat(60));
        assert_eq!(long.chars().count(), 2 + 1 + 60 + 1 + 3);
    }

    #[test]
    fn kv_pads_the_key_to_twenty_two() {
        assert_eq!(
            kv("Default model", "p / m"),
            "  Default model          p / m"
        );
        assert_eq!(
            kv("Reasoning", "default"),
            "  Reasoning              default"
        );
    }

    #[test]
    fn a_table_sizes_columns_to_content_and_joins_with_two_spaces() {
        let rows = vec![
            vec!["openai".to_owned(), "on".to_owned(), "2".to_owned()],
            vec!["x".to_owned(), "off".to_owned(), "1".to_owned()],
        ];
        let lines = table(&["PROVIDER", "STATE", "KEYS"], &rows);
        // Content without the trailing padding the source's `padEnd` emits.
        let trimmed: Vec<&str> = lines.iter().map(|line| line.trim_end()).collect();
        assert_eq!(
            trimmed,
            vec![
                "  PROVIDER  STATE  KEYS",
                "  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}  \u{2500}\u{2500}\u{2500}\u{2500}",
                "  openai    on     2",
                "  x         off    1",
            ]
        );
        // Every cell is padded to its column, last column included (ui.ts
        // L56: `padEnd` then `join('  ')`) — so the widths are pinned too.
        assert_eq!(lines[0].chars().count(), 2 + 8 + 2 + 5 + 2 + 4);
        assert_eq!(lines[2].chars().count(), 2 + 8 + 2 + 5 + 2 + 4);
        assert_eq!(lines[3].chars().count(), 2 + 8 + 2 + 5 + 2 + 4);
    }

    #[test]
    fn an_empty_table_says_so() {
        assert_eq!(table(&["A"], &[]), vec!["  (nothing to show)".to_owned()]);
    }
}
