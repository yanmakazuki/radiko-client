//! Read-only output-device diagnostic. The CPAL query runs on a worker thread.
use super::notices::{self, NoticeLevel};
use radiko::player::output::{OutputCapability, OutputError};
use ratatui::{
    Frame,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::{
    sync::{Arc, mpsc},
    thread,
};

pub(super) const NOTE: &str = "出力機器は OS の既定に従います。アプリ内では選べません";
pub(super) const GUIDANCE: &str =
    "OSのサウンド設定を確認し、d で閉じてから Enter で再生し直してください";
pub(super) const NO_DEVICE_SENTENCE: &str = "既定の音声出力機器がありません";
pub(super) const LOADING: &str = "取得中…";
pub(super) const QUERY_FAILED_PREFIX: &str = "出力機器を確認できません。";

pub(super) fn query_failed_sentence(error: &str) -> String {
    format!("{QUERY_FAILED_PREFIX}{error}")
}

/// Swappable output query. Production calls CPAL; tests record the calling thread.
pub(super) trait DeviceProbe: Send + Sync {
    fn inquire(&self, sample_rate: u32) -> Result<DeviceReport, OutputError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeviceReport {
    pub host: String,
    pub name: String,
    pub sample_rate: u32,
    pub f32_mono: bool,
    pub f32_stereo: bool,
}

impl From<OutputCapability> for DeviceReport {
    fn from(found: OutputCapability) -> Self {
        Self {
            host: found.host,
            name: found.name,
            sample_rate: found.sample_rate,
            f32_mono: found.f32_mono,
            f32_stereo: found.f32_stereo,
        }
    }
}

pub(super) struct CpalProbe;

impl DeviceProbe for CpalProbe {
    fn inquire(&self, sample_rate: u32) -> Result<DeviceReport, OutputError> {
        radiko::player::output::diagnose_default_output(sample_rate).map(DeviceReport::from)
    }
}

pub(super) struct DeviceMessage {
    pub generation: u64,
    pub outcome: Result<DeviceReport, OutputError>,
}

pub(super) fn spawn_inquiry(
    probe: Arc<dyn DeviceProbe>,
    sample_rate: u32,
    generation: u64,
) -> mpsc::Receiver<DeviceMessage> {
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let outcome = probe.inquire(sample_rate);
        let _ = tx.send(DeviceMessage {
            generation,
            outcome,
        });
    });
    rx
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DeviceFailure {
    NoDevice,
    Query(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DeviceStatus {
    Idle,
    Loading,
    Ready(DeviceReport),
    Failed(DeviceFailure),
}

pub(super) fn status_from(outcome: Result<DeviceReport, OutputError>) -> DeviceStatus {
    match outcome {
        Ok(report) => DeviceStatus::Ready(report),
        Err(OutputError::NoDevice) => DeviceStatus::Failed(DeviceFailure::NoDevice),
        Err(error) => DeviceStatus::Failed(DeviceFailure::Query(query_failed_sentence(
            &error.to_string(),
        ))),
    }
}

pub(super) struct Summary {
    pub text: String,
    pub error: bool,
}

pub(super) fn summary(status: &DeviceStatus, last_error: Option<&str>) -> Summary {
    match status {
        DeviceStatus::Idle => Summary {
            text: "機器: 未取得".into(),
            error: false,
        },
        DeviceStatus::Loading => Summary {
            text: LOADING.into(),
            error: false,
        },
        DeviceStatus::Failed(DeviceFailure::NoDevice) => Summary {
            text: format!(
                "{}{NO_DEVICE_SENTENCE}",
                notices::notice_mark(NoticeLevel::Error)
            ),
            error: true,
        },
        DeviceStatus::Failed(DeviceFailure::Query(sentence)) => Summary {
            text: format!("{}{sentence}", notices::notice_mark(NoticeLevel::Error)),
            error: true,
        },
        DeviceStatus::Ready(report) => Summary {
            text: format!(
                "{} / {} / {}",
                report.host,
                report.name,
                last_error.unwrap_or("なし")
            ),
            error: false,
        },
    }
}

pub(super) fn output_error_label(kind: radiko::app::playback::OutputFailure) -> String {
    use radiko::app::playback::OutputFailure;
    match kind {
        OutputFailure::NoDevice => OutputError::NoDevice,
        OutputFailure::UnsupportedFormat => OutputError::UnsupportedFormat,
        OutputFailure::Device => OutputError::Device,
        OutputFailure::Stalled => OutputError::Stalled,
    }
    .to_string()
}

pub(super) fn draw(
    frame: &mut Frame,
    status: &DeviceStatus,
    last_error: Option<&str>,
    no_color: bool,
) {
    let area = frame.area();
    let block = Block::default().borders(Borders::ALL).title("出力機器");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let text = panel_text(status, last_error, no_color, usize::from(inner.width));
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
}

fn panel_text(
    status: &DeviceStatus,
    last_error: Option<&str>,
    no_color: bool,
    width: usize,
) -> ratatui::text::Text<'static> {
    let mut lines = Vec::new();
    match status {
        DeviceStatus::Idle => lines.push(Line::from("機器: 未取得")),
        DeviceStatus::Loading => lines.push(Line::from(LOADING)),
        DeviceStatus::Failed(DeviceFailure::NoDevice) => {
            lines.push(error_line(NO_DEVICE_SENTENCE, no_color));
            lines.push(Line::from(GUIDANCE));
        }
        DeviceStatus::Failed(DeviceFailure::Query(sentence)) => {
            lines.push(error_line(sentence, no_color));
            lines.push(Line::from(GUIDANCE));
        }
        DeviceStatus::Ready(report) => {
            let mono = if report.f32_mono {
                "対応"
            } else {
                "非対応"
            };
            let stereo = if report.f32_stereo {
                "対応"
            } else {
                "非対応"
            };
            lines.push(Line::from(format!("ホスト: {}", report.host)));
            lines.push(Line::from(fit_line(
                &format!("機器: {}", report.name),
                width,
            )));
            lines.push(Line::from(format!(
                "f32 mono {}Hz: {mono}",
                report.sample_rate
            )));
            lines.push(Line::from(format!(
                "f32 stereo {}Hz: {stereo}",
                report.sample_rate
            )));
            lines.push(Line::from(format!(
                "最後の出力エラー: {}",
                last_error.unwrap_or("なし")
            )));
        }
    }
    lines.push(Line::from(NOTE));
    lines.push(Line::from("q / Esc / d 閉じる"));
    ratatui::text::Text::from(lines)
}

fn error_line(sentence: &str, no_color: bool) -> Line<'static> {
    Line::from(Span::styled(
        format!("{}{sentence}", notices::notice_mark(NoticeLevel::Error)),
        notices::notice_style(NoticeLevel::Error, no_color),
    ))
}

pub(super) fn fit_line(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text_width(text) <= width {
        return text.to_owned();
    }
    let ellipsis = "…";
    let mark = text_width(ellipsis).max(1);
    if width <= mark {
        return ellipsis.chars().take(width).collect();
    }
    let budget = width - mark;
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = text_width(&ch.to_string()).max(1);
        if used + ch_width > budget {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push_str(ellipsis);
    out
}

fn text_width(text: &str) -> usize {
    Span::raw(text).width()
}
