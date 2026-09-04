//! Human-readable output helpers. Colour follows `NO_COLOR` and whether
//! stdout is a terminal.

use owo_colors::{OwoColorize, Stream};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl Status {
    pub fn badge(self) -> String {
        match self {
            Status::Ok => format!(
                "[{}]",
                " OK ".if_supports_color(Stream::Stdout, |t| t.green())
            ),
            Status::Warn => format!(
                "[{}]",
                "WARN".if_supports_color(Stream::Stdout, |t| t.yellow())
            ),
            Status::Fail => format!(
                "[{}]",
                "FAIL".if_supports_color(Stream::Stdout, |t| t.red())
            ),
            Status::Skip => format!(
                "[{}]",
                "SKIP".if_supports_color(Stream::Stdout, |t| t.dimmed())
            ),
        }
    }
}

pub fn dim(text: &str) -> String {
    text.if_supports_color(Stream::Stdout, |t| t.dimmed())
        .to_string()
}

/// Renders rows as space-aligned columns under `headers`.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate().take(widths.len()) {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let render = |cells: &[String]| -> String {
        let line = cells
            .iter()
            .enumerate()
            .map(|(index, cell)| format!("{cell:<width$}", width = widths[index]))
            .collect::<Vec<_>>()
            .join("  ");
        format!("{}\n", line.trim_end())
    };
    let header: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    let mut out = render(&header);
    for row in rows {
        out.push_str(&render(row));
    }
    out
}

/// Renders aligned `Key  value` lines.
pub fn key_values(pairs: &[(&str, String)]) -> String {
    let width = pairs
        .iter()
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    pairs
        .iter()
        .map(|(key, value)| format!("{key:<width$}  {value}\n"))
        .collect()
}

/// Formats a byte count with a binary unit and one decimal.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_values_align_on_the_longest_key() {
        assert_eq!(
            key_values(&[("Product", "cm01_se".into()), ("OS", "Android".into())]),
            "Product  cm01_se\nOS       Android\n"
        );
    }

    #[test]
    fn human_bytes_uses_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1_572_864), "1.5 MiB");
        assert_eq!(human_bytes(11_681_792 * 1024), "11.1 GiB");
    }

    #[test]
    fn table_aligns_columns_and_trims_trailing_space() {
        let rows = vec![
            vec!["a".to_string(), "long cell".to_string()],
            vec!["bbb".to_string(), "x".to_string()],
        ];
        assert_eq!(
            table(&["ID", "VALUE"], &rows),
            "ID   VALUE\na    long cell\nbbb  x\n"
        );
    }
}
