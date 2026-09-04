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

#[cfg(test)]
mod tests {
    use super::*;

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
