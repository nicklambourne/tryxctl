//! The PASE overlay: fixed label groups positioned on the 2240×1080 canvas,
//! and the metric batches that update their values. Ported from upstream's
//! `appendPaseOverlayArea` and `sendPaseMetricBatch` with the same ids.

use tryx_proto::wire::v1 as wire;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayArea {
    /// Up to three of the labels in [`METRICS`].
    pub metrics: Vec<String>,
    /// `CPU Badge` and/or `GPU Badge`.
    pub badges: Vec<String>,
    /// `Left`, `Center`, or `Right`.
    pub alignment: String,
    /// 0xRRGGBB.
    pub text_color: u32,
    /// `Top` or `Bottom`, used in waterfall mode.
    pub vertical_placement: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayConfig {
    pub left: OverlayArea,
    pub right: OverlayArea,
    pub dual: bool,
    pub waterfall: bool,
    pub cpu_badge: String,
    pub gpu_badge: String,
}

/// A live value for one label.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricValue {
    pub name: String,
    pub value: String,
    pub unit: String,
}

pub struct Metric {
    pub name: &'static str,
    title: &'static str,
    unit: &'static str,
    group: u32,
    title_id: u32,
    value_id: u32,
    unit_id: u32,
    date_time: bool,
}

/// The firmware's label catalog with its group and label ids.
pub const METRICS: [Metric; 11] = [
    Metric {
        name: "CPU Temperature",
        title: "CPU TEMP",
        unit: "°C",
        group: 100,
        title_id: 101,
        value_id: 102,
        unit_id: 103,
        date_time: false,
    },
    Metric {
        name: "CPU Frequency",
        title: "CPU Frequency",
        unit: "MHZ",
        group: 101,
        title_id: 104,
        value_id: 105,
        unit_id: 106,
        date_time: false,
    },
    Metric {
        name: "CPU Usage",
        title: "CPU Usage",
        unit: "%",
        group: 102,
        title_id: 107,
        value_id: 108,
        unit_id: 109,
        date_time: false,
    },
    Metric {
        name: "CPU Power",
        title: "CPU Power",
        unit: "W",
        group: 103,
        title_id: 110,
        value_id: 111,
        unit_id: 112,
        date_time: false,
    },
    Metric {
        name: "GPU Temperature",
        title: "GPU TEMP",
        unit: "°C",
        group: 104,
        title_id: 113,
        value_id: 114,
        unit_id: 115,
        date_time: false,
    },
    Metric {
        name: "GPU Frequency",
        title: "GPU Frequency",
        unit: "MHZ",
        group: 105,
        title_id: 116,
        value_id: 117,
        unit_id: 118,
        date_time: false,
    },
    Metric {
        name: "GPU Usage",
        title: "GPU Usage",
        unit: "%",
        group: 106,
        title_id: 119,
        value_id: 120,
        unit_id: 121,
        date_time: false,
    },
    Metric {
        name: "GPU Power",
        title: "GPU Power",
        unit: "W",
        group: 107,
        title_id: 122,
        value_id: 123,
        unit_id: 124,
        date_time: false,
    },
    Metric {
        name: "Memory Frequency",
        title: "Memory Frequency",
        unit: "MHZ",
        group: 108,
        title_id: 125,
        value_id: 126,
        unit_id: 127,
        date_time: false,
    },
    Metric {
        name: "Memory Usage",
        title: "Memory Usage",
        unit: "%",
        group: 109,
        title_id: 128,
        value_id: 129,
        unit_id: 130,
        date_time: false,
    },
    Metric {
        name: "Date&Time",
        title: "",
        unit: "",
        group: 110,
        title_id: 131,
        value_id: 132,
        unit_id: 0,
        date_time: true,
    },
];

const SCREEN_WIDTH: i32 = 2240;
const SCREEN_HEIGHT: i32 = 1080;
const TEXT_OFFSET_X: i32 = 60;
const TEXT_OFFSET_Y: i32 = -20;
const VALUE_TEXT_SIZE: i32 = 160;
const TAG_OFFSET_Y: i32 = 70;
const FONT: &str = "roboto-regular";

fn selected(area: &OverlayArea) -> Vec<&'static Metric> {
    let mut out: Vec<&Metric> = Vec::new();
    for name in &area.metrics {
        let name = name.trim();
        if let Some(metric) = METRICS.iter().find(|m| m.name == name)
            && !out.iter().any(|m| m.name == metric.name)
        {
            out.push(metric);
            if out.len() == 3 {
                break;
            }
        }
    }
    out
}

fn alignment(text: &str) -> wire::overlay_group::TextAlignment {
    use wire::overlay_group::TextAlignment;
    if text.eq_ignore_ascii_case("Center") {
        TextAlignment::AlignCenter
    } else if text.eq_ignore_ascii_case("Right") {
        TextAlignment::AlignRight
    } else {
        TextAlignment::AlignLeft
    }
}

fn label(
    id: u32,
    line: u32,
    gap_left: i32,
    size: u32,
    color: u32,
    text: &str,
) -> wire::OverlayLabel {
    wire::OverlayLabel {
        label_id: id,
        line,
        gap_left,
        text_font: FONT.to_string(),
        text_size: size,
        text_color: color,
        text: text.to_string(),
        ..Default::default()
    }
}

fn badge_colors(text: &str) -> (u32, u32) {
    let lower = text.to_ascii_lowercase();
    if lower.contains("nvidia") {
        (0x00629A00, 0x0079AB51)
    } else if lower.contains("intel") {
        (0x000068B5, 0x00566D98)
    } else if lower.contains("amd") || lower.contains("radeon") || lower.contains("ryzen") {
        (0x00A92F2C, 0x00CB6236)
    } else {
        (0x004A4A4A, 0x00707070)
    }
}

fn badge(id: u32, gap_left: i32, text: &str) -> wire::OverlayLabel {
    let (background, gradient) = badge_colors(text);
    wire::OverlayLabel {
        label_id: id,
        gap_left,
        background: wire::overlay_label::Background::GradientHorizontal as i32,
        background_color: background,
        gradient_color: gradient,
        text_font: FONT.to_string(),
        text_size: 30,
        text_color: 0x00DCDCDC,
        text: format!("  {text}  "),
        ..Default::default()
    }
}

/// Local date and time as the panel shows them.
fn now_text() -> (String, String) {
    let offset_ms = crate_local_offset_ms();
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + offset_ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (
        format!("{:02}/{:02}/{}", day, month, year),
        format!("{:02}:{:02}", rem / 3600, (rem % 3600) / 60),
    )
}

fn crate_local_offset_ms() -> i64 {
    // The daemon and CLI add the host offset themselves; here only the
    // clock label needs it, so read the TZ offset through libc.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    tz_offset_seconds(now) * 1000
}

#[cfg(unix)]
fn tz_offset_seconds(unix: i64) -> i64 {
    unsafe extern "C" {
        fn localtime_r(t: *const i64, tm: *mut [i64; 16]) -> *mut [i64; 16];
    }
    // tm_gmtoff sits at index 10 of the glibc/musl/darwin struct tm when
    // viewed as i64 slots on 64-bit targets; fall back to zero elsewhere.
    if std::mem::size_of::<usize>() != 8 {
        return 0;
    }
    let t = unix;
    let mut tm = [0i64; 16];
    // SAFETY: the buffer is larger than struct tm on every 64-bit unix.
    let ok = unsafe { !localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return 0;
    }
    // struct tm: 9 ints (36 bytes) then tm_gmtoff (long) at offset 40.
    let bytes: [u8; 128] = unsafe { std::mem::transmute(tm) };
    i64::from_ne_bytes(bytes[40..48].try_into().unwrap())
}

#[cfg(not(unix))]
fn tz_offset_seconds(_unix: i64) -> i64 {
    0
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn append_area(
    layout: &mut wire::OverlayLayout,
    overlay: &OverlayConfig,
    area: &OverlayArea,
    right: bool,
) {
    if area.metrics.is_empty() && area.badges.is_empty() {
        return;
    }
    let area_count = if overlay.dual { 2 } else { 1 };
    let area_x = if right {
        SCREEN_WIDTH / area_count + TEXT_OFFSET_X
    } else {
        TEXT_OFFSET_X
    };
    let id_offset = if right { 100 } else { 0 };
    let align = alignment(&area.alignment);
    let title_gap = if align == wire::overlay_group::TextAlignment::AlignLeft {
        13
    } else {
        0
    };
    let chosen = selected(area);
    let count = chosen.len() as i32;
    let bottom = area.vertical_placement.eq_ignore_ascii_case("Bottom");
    let (date, time) = now_text();

    for (index, metric) in chosen.iter().enumerate() {
        let index = index as i32;
        let mut group_y = if count == 1 {
            SCREEN_HEIGHT / 2
        } else {
            (SCREEN_HEIGHT / (count + 1)) * (index + 1) + index * 10
        };
        group_y -= VALUE_TEXT_SIZE / 2;
        let mut group_width = SCREEN_WIDTH / area_count - TEXT_OFFSET_X * 2;
        let mut group_x = area_x;
        if overlay.waterfall {
            group_width = SCREEN_WIDTH / 2 - TEXT_OFFSET_X * 2 - 50;
            if overlay.dual {
                if right {
                    group_x = TEXT_OFFSET_X;
                } else {
                    group_y += SCREEN_WIDTH / 2;
                }
            } else if bottom {
                group_y += SCREEN_WIDTH / 2;
            }
        }
        let mut group = wire::OverlayGroup {
            group_id: metric.group + id_offset,
            group_x: group_x as u32,
            group_y: (group_y + TEXT_OFFSET_Y).max(0) as u32,
            group_width: group_width as u32,
            group_height: 160,
            text_align: align as i32,
            line_gap: -10,
            labels: Vec::new(),
        };
        let title_id = metric.title_id + id_offset;
        let value_id = metric.value_id + id_offset;
        let unit_id = if metric.unit_id == 0 {
            0
        } else {
            metric.unit_id + id_offset
        };
        if metric.date_time {
            group
                .labels
                .push(label(title_id, 1, title_gap, 30, area.text_color, &date));
            group
                .labels
                .push(label(value_id, 0, 0, 160, area.text_color, &time));
        } else {
            group.labels.push(label(
                title_id,
                1,
                title_gap,
                30,
                area.text_color,
                metric.title,
            ));
            group
                .labels
                .push(label(value_id, 0, 0, 160, area.text_color, "--"));
            group
                .labels
                .push(label(unit_id, 0, 0, 36, area.text_color, metric.unit));
        }
        layout.label_groups.push(group);
    }

    if area.badges.is_empty() {
        return;
    }
    let mut badge_y = TAG_OFFSET_Y;
    let mut badge_width = SCREEN_WIDTH / area_count - TEXT_OFFSET_X * 2;
    let mut badge_x = area_x + 10;
    if overlay.waterfall {
        badge_width = SCREEN_WIDTH / 2 - TEXT_OFFSET_X * 2 - 30;
        if overlay.dual {
            if right {
                badge_x = TEXT_OFFSET_X + 10;
            } else {
                badge_y += SCREEN_WIDTH / 2;
            }
        } else if bottom {
            badge_y += SCREEN_WIDTH / 2;
        }
    }
    let base = if right { 400 } else { 300 };
    let mut group = wire::OverlayGroup {
        group_id: base,
        group_x: badge_x as u32,
        group_y: badge_y as u32,
        group_width: badge_width as u32,
        text_align: align as i32,
        line_gap: 1,
        ..Default::default()
    };
    let mut index = 0;
    for item in &area.badges {
        let cpu = item.eq_ignore_ascii_case("CPU Badge") || item.eq_ignore_ascii_case("cpu");
        let gpu = item.eq_ignore_ascii_case("GPU Badge") || item.eq_ignore_ascii_case("gpu");
        if !cpu && !gpu {
            continue;
        }
        let text = if cpu {
            if overlay.cpu_badge.is_empty() {
                "CPU"
            } else {
                &overlay.cpu_badge
            }
        } else if overlay.gpu_badge.is_empty() {
            "GPU"
        } else {
            &overlay.gpu_badge
        };
        group.labels.push(badge(
            base + if cpu { 1 } else { 2 },
            if index > 0 { 10 } else { 0 },
            text,
        ));
        index += 1;
    }
    // Upstream adds the badge group even when no badge name was recognised.
    layout.label_groups.push(group);
}

/// The overlay layout ("run config") for the current selection.
pub fn run_config(overlay: &OverlayConfig) -> wire::OverlayLayout {
    let mut layout = wire::OverlayLayout::default();
    append_area(&mut layout, overlay, &overlay.left, false);
    if overlay.dual {
        append_area(&mut layout, overlay, &overlay.right, true);
    }
    layout
}

fn push_update(batch: &mut wire::MetricBatch, group: u32, label_id: u32, text: &str) {
    batch.label_groups.push(wire::OverlayGroupUpdate {
        group_id: group,
        label_texts: vec![wire::OverlayLabelUpdate {
            label_id,
            text: text.to_string(),
        }],
    });
}

/// Value updates for the selected labels; `None` when nothing applies.
pub fn metric_batch(overlay: &OverlayConfig, values: &[MetricValue]) -> Option<wire::MetricBatch> {
    let mut batch = wire::MetricBatch::default();
    let (date, time) = now_text();
    let mut append = |area: &OverlayArea, offset: u32| {
        for metric in selected(area) {
            let group = metric.group + offset;
            if metric.date_time {
                push_update(&mut batch, group, metric.title_id + offset, &date);
                push_update(&mut batch, group, metric.value_id + offset, &time);
                continue;
            }
            let Some(value) = values
                .iter()
                .find(|v| v.name == metric.name && !v.value.is_empty())
            else {
                continue;
            };
            push_update(&mut batch, group, metric.value_id + offset, &value.value);
            // Only the temperature groups carry a unit update, as upstream.
            if metric.group == 100 || metric.group == 104 {
                let unit = if value.unit.is_empty() {
                    metric.unit
                } else {
                    &value.unit
                };
                push_update(&mut batch, group, metric.unit_id + offset, unit);
            }
        }
    };
    append(&overlay.left, 0);
    if overlay.dual {
        append(&overlay.right, 100);
    }
    (!batch.label_groups.is_empty()).then_some(batch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay() -> OverlayConfig {
        OverlayConfig {
            left: OverlayArea {
                metrics: vec![
                    "CPU Temperature".into(),
                    "GPU Usage".into(),
                    "Bogus".into(),
                    "CPU Temperature".into(),
                ],
                badges: vec!["CPU Badge".into(), "gpu".into()],
                alignment: "Left".into(),
                text_color: 0x00DCDCDC,
                vertical_placement: "Top".into(),
            },
            cpu_badge: "AMD Ryzen 9".into(),
            gpu_badge: "NVIDIA RTX".into(),
            ..OverlayConfig::default()
        }
    }

    #[test]
    fn layout_uses_upstream_ids_positions_and_fonts() {
        let layout = run_config(&overlay());
        assert_eq!(
            layout.label_groups.len(),
            3,
            "two metrics and one badge group"
        );
        let cpu = &layout.label_groups[0];
        assert_eq!(cpu.group_id, 100);
        assert_eq!(
            (cpu.group_x, cpu.group_y, cpu.group_width, cpu.group_height),
            (60, 260, 2120, 160)
        );
        assert_eq!(cpu.line_gap, -10);
        assert_eq!(
            cpu.labels.iter().map(|l| l.label_id).collect::<Vec<_>>(),
            vec![101, 102, 103]
        );
        assert_eq!(cpu.labels[0].text, "CPU TEMP");
        assert_eq!(
            cpu.labels[0].gap_left, 13,
            "left alignment gets the title gap"
        );
        assert_eq!(cpu.labels[1].text_size, 160);
        assert_eq!(cpu.labels[2].text, "°C");
        assert!(cpu.labels.iter().all(|l| l.text_font == "roboto-regular"));
        let gpu = &layout.label_groups[1];
        assert_eq!(gpu.group_id, 106);
        assert_eq!(gpu.group_y, 720 + 10 - 80 - 20);
        let badges = &layout.label_groups[2];
        assert_eq!(badges.group_id, 300);
        assert_eq!(
            badges.labels.iter().map(|l| l.label_id).collect::<Vec<_>>(),
            vec![301, 302]
        );
        assert_eq!(badges.labels[0].text, "  AMD Ryzen 9  ");
        assert_eq!(badges.labels[0].background_color, 0x00A92F2C);
        assert_eq!(badges.labels[1].background_color, 0x00629A00);
        assert_eq!(badges.labels[1].gap_left, 10);
    }

    #[test]
    fn dual_and_waterfall_shift_the_right_area() {
        let mut config = overlay();
        config.dual = true;
        config.right = OverlayArea {
            metrics: vec!["Date&Time".into()],
            alignment: "Right".into(),
            ..OverlayArea::default()
        };
        let layout = run_config(&config);
        let clock = layout
            .label_groups
            .iter()
            .find(|g| g.group_id == 210)
            .expect("right area clock group");
        assert_eq!(clock.group_x, 1180);
        assert_eq!(clock.labels.len(), 2);
        assert_eq!(clock.labels[0].label_id, 231);
        assert_eq!(
            clock.text_align,
            wire::overlay_group::TextAlignment::AlignRight as i32
        );
        config.waterfall = true;
        let waterfall = run_config(&config);
        let left = waterfall
            .label_groups
            .iter()
            .find(|g| g.group_id == 100)
            .unwrap();
        assert_eq!(left.group_width, 950);
        assert!(
            left.group_y > 1000,
            "left area moves down the rotated canvas"
        );
    }

    #[test]
    fn metric_batch_updates_values_and_temperature_units_only() {
        let values = vec![
            MetricValue {
                name: "CPU Temperature".into(),
                value: "54".into(),
                unit: String::new(),
            },
            MetricValue {
                name: "GPU Usage".into(),
                value: "3".into(),
                unit: "%".into(),
            },
        ];
        let batch = metric_batch(&overlay(), &values).unwrap();
        let updates: Vec<(u32, u32, &str)> = batch
            .label_groups
            .iter()
            .map(|g| {
                (
                    g.group_id,
                    g.label_texts[0].label_id,
                    g.label_texts[0].text.as_str(),
                )
            })
            .collect();
        assert_eq!(
            updates,
            vec![(100, 102, "54"), (100, 103, "°C"), (106, 120, "3")]
        );
        assert!(metric_batch(&OverlayConfig::default(), &values).is_none());
    }

    #[test]
    fn civil_dates_are_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_700), (2026, 9, 4));
    }
}
