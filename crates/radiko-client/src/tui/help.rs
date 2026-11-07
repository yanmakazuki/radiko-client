//! Read-only help; acquisition and playback continue in the owning loop.
use super::scroll::Scroll;
#[cfg(test)]
use crossterm::event::KeyCode;
use radiko::player::state::PlaybackSnapshot;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub(super) struct HelpExtras<'a> {
    pub device_summary: &'a str,
    pub device_error: bool,
    pub no_color: bool,
}

pub(super) const KEYS: &str = concat!(
    "番組を探す\n上下 / j k : 番組選択　PageUp / PageDown : 1画面移動\nHome / End : 先頭／末尾（選択だけでは再生しません）\nEnter : 選択枠を再生（ライブ画面ではライブ）\nv : ライブ／タイムフリー画面切替　 l : 選択枠のライブ\ne : 今回の取得状況・未取得の地域／局・日を閲覧\nd : 出力機器の読み取り専用診断。Esc / d / q で閉じ、終了しない\nf : 全条件編集　 / : キーワード編集　 r : 新規取得\n条件のEnterで検索確定、Escで取消。キーワード・条件の各欄・保存名は←→で書記素を1つ、Home/Endで端、Backspaceで前、Deleteで次。確定前は通信しない。キーワード入力中は表示中の一覧の一致件数だけを未確定で出し、一覧は変えず取得しない。地域・分類欄のF2で名称選択。未確定の変換は扱わない。\nb : 名前付き条件の保存・呼び出し（呼び出し後も検索確定が必要）\nn 放送中の番組へ移動 o 選択局の条件を編集\n再生する\nSpace : 一時停止／再開　 s : 停止　 + / - : 音量\n左右 : 15秒シーク　 g : 枠内の整数秒を入力（タイムフリーのみ）\nc : 公式AIチャプター。上下・ページ・Home/Endで閲覧、Enterで再生。rで再取得。\nTab : 章一覧／要約のスクロール切替（先頭1200文字まで）\n章の時刻原点を照合できない場合はジャンプできません。\n出力機器のエラーで止まったときは、プレイヤー欄に対処を出します。自動では再生し直しません。\n出力先がないとき：「出力先がありません。OSのサウンド設定を確認し、Enterで再生し直してください」\n対応形式外のとき：「対応形式外です。OSの出力設定を確認し、Enterで再生し直してください」\n機器の開始や情報取得に失敗したとき：「出力機器を確認してください。OSの出力先を見て、Enterで再生し直してください」\nスリープ復帰・機器変更後に出力が止まったとき：「スリープ復帰・機器変更後は出力先を確認し、Enterで再生し直してください」\nどの対処も次の行は「自動再開はしません」。次に再生するか Esc まで残り、ライブ画面では現在の放送枠を選んでください。\n再生が続いているあいだは「再生を停止しました」を出しません。\nフッターの通知は1件です。種類から重要度を決め、高い方を出し、同じときは新しい方を出します。エラーは赤の太字で先頭に✕、警告は黄色で先頭に!、情報は通常の色です。警告は時間では消えず次のキーで消えます。エラーは次の操作まで残ります。情報は5秒または次のキーで消えます。キーワードと条件の入力中は、文字を打ってもエラーは消えません。消えるのは Enter と Esc だけです。エラーの下に隠れた警告は、エラーが消えてから表示され、その次のキーで消えます。NO_COLOR が空でないときは色を付けません。\n終わった番組への切り替えは「番組は終了しています。再生は続いています」。\n切り替え時の出力診断が時間を超えたときは「切り替えられませんでした。再生は続いています」。\n再生中に次の準備が失敗したときも「停止しました」は出さず、音声が届かなければ「切り替え先の音声が届きませんでした。再生は続いています」、配信が途中で終われば「切り替え先の配信が途中で終わりました。再生は続いています」、位置が範囲外なら「指定した位置は番組の範囲外です。再生は続いています」、配信の取得に失敗すれば「切り替え先の配信を取得できませんでした。再生は続いています」、それ以外は「切り替えられませんでした。再生は続いています」。\n再生が続いているあいだは、これらの文に「再生準備失敗（現在の状態を維持）」も「再生切替失敗（現在の状態を維持）」も付けません。\n\n認証と終了\nI : ログイン入力　 O : ログアウト確認　 a : セッション再確認\n未ログインは「未ログインです。I でログイン」。二つのキーを並べる案内は「a で再確認 / I で再ログイン」の順です。\nセッション失効は「セッション失効。a で再確認 / I で再ログイン」。\n認証期限切れで止めた通知は「認証期限切れのため再生を停止しました。a で再確認 / I で再ログイン」。\nプレイヤー行は、ログアウト前が「停止中：認証期限切れ（a で再確認）」、ログアウト後が「停止中：認証期限切れ（I で再ログイン）」です。\n",
    logout_key_wording!(),
    "。パスワードは表示・保存しません。\n",
    quit_key_wording!(),
    "します。",
    dismiss_notice_wording!(),
    "。再生・一時停止・選択・条件は変えません。\n操作一覧を開いている間も検索・再生は継続します。"
);

pub fn draw(
    frame: &mut Frame,
    playback: Option<&PlaybackSnapshot>,
    volume: u32,
    scroll: &mut Scroll,
    last_diagnostics: &str,
    extras: HelpExtras<'_>,
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
    // One summary row once the terminal can keep the help text's previous height.
    // A shorter help pane overlaps wrapped lines in Ratatui 0.29.
    let summary_row = u16::from(frame.area().height >= 20);
    let areas = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(
            5u16.saturating_add(summary_row)
                .saturating_add(u16::try_from(diagnostic_lines).unwrap_or(u16::MAX)),
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
    let active = playback.map(|snapshot| (snapshot, super::playback_view::presentation(snapshot)));
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(areas[1]);
    frame.render_widget(block, areas[1]);
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ];
    if summary_row == 1 {
        constraints.push(Constraint::Length(1));
    }
    let rows = Layout::vertical(constraints).split(inner);
    paint_active(
        frame,
        rows[0],
        active.as_ref().map(|(snapshot, view)| (*snapshot, view)),
        volume,
    );
    frame.render_widget(diagnostic, rows[1]);
    frame.render_widget(
        Paragraph::new(concat!(
            dialog_close_hint!(),
            "\n↑↓ PgUp/Dn Home/End / Ctrl+C 終了"
        )),
        rows[2],
    );
    if summary_row == 1 {
        let summary = if rows[3].width == 0 {
            String::new()
        } else {
            super::device::fit_line(extras.device_summary, usize::from(rows[3].width))
        };
        let summary_line = if extras.device_error {
            Line::from(Span::styled(
                summary,
                super::notices::notice_style(super::notices::NoticeLevel::Error, extras.no_color),
            ))
        } else {
            Line::from(summary)
        };
        frame.render_widget(
            Paragraph::new(summary_line).wrap(Wrap { trim: false }),
            rows[3],
        );
    }
}

fn paint_active(
    frame: &mut Frame,
    area: Rect,
    active: Option<(&PlaybackSnapshot, &super::playback_view::Presentation)>,
    volume: u32,
) {
    let Some((snapshot, view)) = active else {
        frame.render_widget(
            Paragraph::new(format!("再生中: なし / 音量{volume}%")),
            area,
        );
        return;
    };
    let head = format!(
        "再生中: [{} / {}] {}",
        snapshot.info.station, view.mode, view.state
    );
    let prefix = super::playback_view::fit_status_line(
        &head,
        snapshot.volume,
        &view.clock,
        usize::from(area.width),
    );
    let title = snapshot.info.title.as_deref().unwrap_or("番組名未取得");
    let clock_kept = prefix.ends_with(view.clock.as_str());
    if let Some(ratio) = view.ratio.filter(|_| clock_kept) {
        let prefix_width = unicode_width::UnicodeWidthStr::width(prefix.as_str());
        if let Some(bar) = super::playback_view::reserved_bar(area.width, prefix_width) {
            let text_width = u16::try_from(prefix_width).unwrap_or(area.width);
            let columns = Layout::horizontal([
                Constraint::Length(text_width),
                Constraint::Length(1),
                Constraint::Length(bar),
                Constraint::Min(0),
            ])
            .split(area);
            frame.render_widget(Paragraph::new(prefix.as_str()), columns[0]);
            super::playback_view::paint_bar(frame, columns[2], ratio);
            frame.render_widget(Paragraph::new(format!(" {title}")), columns[3]);
            return;
        }
    }
    frame.render_widget(Paragraph::new(format!("{prefix} / {title}")), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn help_extras() -> HelpExtras<'static> {
        HelpExtras {
            device_summary: "機器: 未取得",
            device_error: false,
            no_color: false,
        }
    }

    #[test]
    fn help_key_list_documents_grapheme_editing_for_keyword_conditions_and_names() {
        assert!(
            KEYS.contains(
                "キーワード・条件の各欄・保存名は←→で書記素を1つ、Home/Endで端、Backspaceで前、Deleteで次"
            ),
            "the ? list must describe field editing"
        );
        assert!(KEYS.contains("未確定の変換は扱わない"));
        assert!(KEYS.contains("確定前は通信しない"));
        assert!(
            KEYS.contains(
                "キーワード入力中は表示中の一覧の一致件数だけを未確定で出し、一覧は変えず取得しない"
            ),
            "the ? list must describe the uncommitted keyword count"
        );
    }

    #[test]
    fn help_key_list_documents_on_air_move_and_station_draft() {
        assert!(
            KEYS.contains("n 放送中の番組へ移動"),
            "the ? list must name n"
        );
        assert!(
            KEYS.contains("o 選択局の条件を編集"),
            "the ? list must name o"
        );
    }

    #[test]
    fn help_key_list_documents_the_read_only_device_panel() {
        assert!(
            KEYS.contains("d : 出力機器の読み取り専用診断。Esc / d / q で閉じ、終了しない"),
            "the ? key list must include d"
        );
    }

    #[test]
    fn device_summary_is_hidden_at_19_rows_and_shown_at_20() {
        let hidden = drawn_help(19);
        let shown = drawn_help(20);
        assert!(
            !hidden.contains("DEVICE-BOUNDARY"),
            "19 rows must hide the device line:\n{hidden}"
        );
        assert!(
            shown.contains("DEVICE-BOUNDARY"),
            "20 rows must show the device line:\n{shown}"
        );
    }

    fn drawn_help(height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, height)).unwrap();
        let mut scroll = Scroll::default();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    None,
                    0,
                    &mut scroll,
                    "",
                    HelpExtras {
                        device_summary: "DEVICE-BOUNDARY",
                        device_error: false,
                        no_color: false,
                    },
                )
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn failed_request_without_response_keeps_its_diagnostics_in_help() {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let mut scroll = Scroll::default();
        let diagnostics = "直近通知（終了時未集計）: 通信診断 待機0ms / 応答max250ms / 本文max0ms / 試行1・再試行待ち0 / 通信失敗1・期限1・接続0 / HTTP未取得";
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    None,
                    0,
                    &mut scroll,
                    diagnostics,
                    HelpExtras {
                        device_summary: "機器: 未取得",
                        device_error: false,
                        no_color: false,
                    },
                )
            })
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
            .draw(|frame| draw(frame, None, 0, &mut scroll, "", help_extras()))
            .unwrap();
        assert!(scroll.maximum > 0);
        scroll.key(KeyCode::End);
        terminal
            .draw(|frame| draw(frame, None, 0, &mut scroll, "", help_extras()))
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
        let mut large = Terminal::new(TestBackend::new(160, 47)).unwrap();
        large
            .draw(|frame| draw(frame, None, 0, &mut scroll, "", help_extras()))
            .unwrap();
        assert_eq!(scroll.offset, scroll.maximum);
        assert!(
            scroll.maximum <= 7,
            "160x47 should still show almost all of the help, maximum {}",
            scroll.maximum
        );
        for (width, height) in [(0, 0), (1, 1), (2, 2), (10, 5)] {
            let mut tiny = Terminal::new(TestBackend::new(width, height)).unwrap();
            tiny.draw(|frame| draw(frame, None, 0, &mut scroll, "", help_extras()))
                .unwrap();
            scroll.key(KeyCode::End);
            assert!(scroll.offset <= scroll.maximum);
        }
    }
}
