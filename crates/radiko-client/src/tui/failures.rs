//! Current search failures only. Browsing never starts acquisition or playback.
use super::viewport;
use radiko::{
    app::{AppState, SearchStatus},
    player::state::output_diagnostics,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

pub fn count(app: &AppState) -> usize {
    app.area_failures.len() + app.schedule_failures.len() + app.invalid_schedules.len()
}

pub fn draw(frame: &mut Frame, app: &AppState, list: &mut ListState, height: &mut usize) {
    let panes = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(1),
        Constraint::Length(5),
    ])
    .split(frame.area());
    let status = match app.status {
        SearchStatus::Idle => "未取得",
        SearchStatus::Loading => "取得中（未取得範囲は完了後に確定）",
        SearchStatus::Ready => "取得完了",
        SearchStatus::Partial => "部分取得・未取得あり",
        SearchStatus::Failed => "取得失敗",
    };
    let note = if app.status == SearchStatus::Failed {
        app.notification.as_str()
    } else {
        "不正枠は番組一覧から除外します。今回の取得だけを表示します。"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{}〜{} JST / {status}\n地域失敗{} / 局・日失敗{} / 不正枠{}\n{note}",
            app.committed.range().start(),
            app.committed.range().end(),
            app.area_failures.len(),
            app.schedule_failures.len(),
            app.invalid_count,
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("取得状況・未取得範囲"),
        ),
        panes[0],
    );
    if list.selected().is_none() && count(app) > 0 {
        list.select(Some(0));
    }
    *height = usize::from(panes[1].height.saturating_sub(2));
    let visible = viewport::visible(list, count(app), *height);
    let items: Vec<_> = visible
        .clone()
        .map(|index| {
            let failure = app
                .area_failures
                .get(index)
                .map(|f| ("地域", f))
                .or_else(|| {
                    index
                        .checked_sub(app.area_failures.len())
                        .and_then(|index| app.schedule_failures.get(index))
                        .map(|f| ("番組表", f))
                });
            if let Some((kind, failure)) = failure {
                ListItem::new(format!(
                    "{kind} {} {} / {}",
                    failure.target,
                    failure.day.map(|d| d.to_string()).unwrap_or_default(),
                    failure.error
                ))
            } else {
                let invalid = &app.invalid_schedules
                    [index - app.area_failures.len() - app.schedule_failures.len()];
                ListItem::new(format!(
                    "不正枠除外 {} {} / {}件（正常な枠は表示）",
                    invalid.station_id, invalid.day, invalid.count
                ))
            }
        })
        .collect();
    let mut local = ListState::default();
    local.select(
        list.selected()
            .filter(|i| visible.contains(i))
            .map(|i| i - visible.start),
    );
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("未取得・不正枠除外（局ID・放送日）"),
            )
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("▶ "),
        panes[1],
        &mut local,
    );
    let playback = app
        .playback
        .as_ref()
        .map(|p| {
            format!(
                "再生中: {} / {:?} / 音量{}%\n{}",
                p.info.station,
                p.state,
                p.volume,
                output_diagnostics(p.output.as_ref(), p.segments)
            )
        })
        .unwrap_or_else(|| "再生中: なし".into());
    frame.render_widget(
        Paragraph::new(format!(
            "{playback}\n上下・ページ・Home/End 選択 / e / {} / Ctrl+C 終了",
            dialog_close_hint!()
        ))
        .block(Block::default().borders(Borders::ALL)),
        panes[2],
    );
}
