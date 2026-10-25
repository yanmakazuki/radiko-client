//! Read-only help; acquisition and playback continue in the owning loop.
use super::scroll::Scroll;
#[cfg(test)]
use crossterm::event::KeyCode;
use radiko::player::state::PlaybackSnapshot;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    widgets::{Block, Borders, Paragraph, Wrap},
};

const KEYS: &str = "番組を探す\n上下 / j k : 番組選択　PageUp / PageDown : 1画面移動\nHome / End : 先頭／末尾（選択だけでは再生しません）\nEnter : 選択枠を再生（ライブ画面ではライブ）\nv : ライブ／タイムフリー画面切替　 l : 選択枠のライブ\ne : 今回の取得状況・未取得の地域／局・日を閲覧\nf : 全条件編集　 / : キーワード編集　 r : 新規取得\n条件のEnterで検索確定、Escで取消。地域・分類欄のF2で名称選択。\nb : 名前付き条件の保存・呼び出し（呼び出し後も検索確定が必要）\n\n再生する\nSpace : 一時停止／再開　 s : 停止　 + / - : 音量\n左右 : 15秒シーク　 g : 枠内の整数秒を入力（タイムフリーのみ）\nc : 公式AIチャプター。上下・ページ・Home/Endで閲覧、Enterで再生。rで再取得。\nTab : 章一覧／要約のスクロール切替（先頭1200文字まで）\n章の時刻原点を照合できない場合はジャンプできません。\nスリープ復帰・機器変更後に出力停止した場合：\nMacの出力先を確認し、番組一覧で対象を選びEnterで再生し直してください。\n自動再生はしません。ライブ画面では現在の放送枠を選んでください。\n\n認証と終了\nI : ログイン入力　 O : ログアウト確認　 a : セッション再確認\nログアウトはyで確定、n / Escで取消。パスワードは表示・保存しません。\n通常画面のq / Esc、またはCtrl+Cで終了します。\n操作一覧を開いている間も検索・再生は継続します。";

pub fn draw(
    frame: &mut Frame,
    playback: Option<&PlaybackSnapshot>,
    volume: u32,
    scroll: &mut Scroll,
    last_diagnostics: &str,
) {
    let diagnostics = playback
        .and_then(|playback| playback.pipeline.as_ref())
        .map(|pipeline| {
            if pipeline.http.is_some() {
                pipeline.lines()
            } else {
                pipeline.label()
            }
        })
        .or_else(|| {
            let lines: Vec<_> = last_diagnostics
                .lines()
                .filter(|line| {
                    (line.contains("通信診断") && !line.ends_with("通信診断 未取得"))
                        || (line.contains("処理診断") && !line.ends_with("処理診断 未取得"))
                })
                .collect();
            (!lines.is_empty()).then(|| lines.join("\n"))
        });
    let diagnostic =
        Paragraph::new(diagnostics.as_deref().unwrap_or_default()).wrap(Wrap { trim: false });
    let diagnostic_lines = diagnostics.as_ref().map_or(0, |_| {
        diagnostic.line_count(frame.area().width.saturating_sub(2))
    });
    let areas = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(
            5u16.saturating_add(u16::try_from(diagnostic_lines).unwrap_or(u16::MAX)),
        ),
    ])
    .split(frame.area());
    let paragraph = Paragraph::new(KEYS).wrap(Wrap { trim: false });
    // Ratatui 0.29's opt-in line-info API expects the text's inner width.
    let lines = paragraph.line_count(areas[0].width.saturating_sub(2));
    scroll.fit(lines, areas[0].height.saturating_sub(2));
    frame.render_widget(
        paragraph
            .scroll((scroll.offset, 0))
            .block(Block::default().borders(Borders::ALL).title(format!(
                "操作一覧 {}/{}行",
                if lines == 0 {
                    0
                } else {
                    scroll.offset.saturating_add(1)
                },
                lines
            ))),
        areas[0],
    );
    let active = playback
        .map(|p| {
            format!(
                "再生中: [{} / {:?}] {:?} / 音量{}% / {}",
                p.info.station,
                p.info.mode,
                p.state,
                p.volume,
                p.info.title.as_deref().unwrap_or("番組名未取得"),
            )
        })
        .unwrap_or_else(|| format!("再生中: なし / 音量{volume}%"));
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(areas[1]);
    frame.render_widget(block, areas[1]);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(inner);
    frame.render_widget(Paragraph::new(active), rows[0]);
    frame.render_widget(diagnostic, rows[1]);
    frame.render_widget(
        Paragraph::new("Esc / ? / q 操作一覧を閉じる\n↑↓ PgUp/Dn Home/End / Ctrl+C 終了"),
        rows[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn failed_request_without_response_keeps_its_diagnostics_in_help() {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let mut scroll = Scroll::default();
        let diagnostics = "直近通知（終了時未集計）: 通信診断 待機0ms / 応答max250ms / 本文max0ms / 試行1・再試行待ち0 / 通信失敗1・期限1・接続0 / HTTP未取得";
        terminal
            .draw(|frame| draw(frame, None, 0, &mut scroll, diagnostics))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        let text = text.replace([' ', '│'], "");
        assert!(text.contains("通信失敗1・期限1・接続0"));
        assert!(text.contains("HTTP未取得"));
    }

    #[test]
    fn wrapped_help_reaches_the_end_and_clamps_after_resize() {
        let mut scroll = Scroll::default();
        let mut terminal = Terminal::new(TestBackend::new(48, 16)).unwrap();
        terminal
            .draw(|frame| draw(frame, None, 0, &mut scroll, ""))
            .unwrap();
        assert!(scroll.maximum > 0);
        scroll.key(KeyCode::End);
        terminal
            .draw(|frame| draw(frame, None, 0, &mut scroll, ""))
            .unwrap();
        let body: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .take(48 * 11)
            .map(|cell| cell.symbol())
            .collect();
        assert!(body.replace([' ', '│'], "").contains("継続します。"));
        let maximum = scroll.offset;
        scroll.key(KeyCode::PageDown);
        assert_eq!(scroll.offset, maximum);
        scroll.key(KeyCode::PageUp);
        assert!(scroll.offset < maximum);
        scroll.key(KeyCode::Home);
        assert_eq!(scroll.offset, 0);
        scroll.key(KeyCode::End);
        let mut large = Terminal::new(TestBackend::new(160, 45)).unwrap();
        large
            .draw(|frame| draw(frame, None, 0, &mut scroll, ""))
            .unwrap();
        assert_eq!(scroll.offset, 0);
        assert_eq!(scroll.maximum, 0);
        for (width, height) in [(0, 0), (1, 1), (2, 2), (10, 5)] {
            let mut tiny = Terminal::new(TestBackend::new(width, height)).unwrap();
            tiny.draw(|frame| draw(frame, None, 0, &mut scroll, ""))
                .unwrap();
            scroll.key(KeyCode::End);
            assert!(scroll.offset <= scroll.maximum);
        }
    }
}
