//! Read-only help; acquisition and playback continue in the owning loop.
use radiko_client::player::state::PlaybackSnapshot;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    widgets::{Block, Borders, Paragraph, Wrap},
};

const KEYS: &str = "番組を探す\n上下 / j k : 番組選択（選択だけでは再生しません）\nEnter : 選択枠を再生（ライブ画面ではライブ）\nv : ライブ／タイムフリー画面切替　 l : 選択枠のライブ\nf : 全条件編集　 / : キーワード編集　 r : 新規取得\n条件のEnterで検索確定、Escで取消。地域・分類欄のF2で名称選択。\nb : 名前付き条件の保存・呼び出し（呼び出し後も検索確定が必要）\n\n再生する\nSpace : 一時停止／再開　 s : 停止　 + / - : 音量\n左右 : 15秒シーク　 g : 枠内の整数秒を入力（タイムフリーのみ）\nc : 公式AIチャプター。上下で閲覧、Enterで章から再生、rで再取得。\n章の時刻原点を照合できない場合はジャンプできません。\n\n認証と終了\nI : ログイン入力　 O : ログアウト確認　 a : セッション再確認\nログアウトはyで確定、n / Escで取消。パスワードは表示・保存しません。\n通常画面のq / Esc、またはCtrl+Cで終了します。\n操作一覧を開いている間も検索・再生は継続します。";

pub fn draw(frame: &mut Frame, playback: Option<&PlaybackSnapshot>, volume: u32) {
    let areas = Layout::vertical([Constraint::Min(1), Constraint::Length(5)]).split(frame.area());
    frame.render_widget(
        Paragraph::new(KEYS)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("操作一覧")),
        areas[0],
    );
    let active = playback
        .map(|p| {
            format!(
                "再生中: {} / {} / {:?} / {:?} / 音量{}%",
                p.info.title.as_deref().unwrap_or("番組名未取得"),
                p.info.station,
                p.info.mode,
                p.state,
                p.volume
            )
        })
        .unwrap_or_else(|| format!("再生中: なし / 音量{volume}%"));
    frame.render_widget(
        Paragraph::new(format!(
            "{active}\nEsc / ? / q 操作一覧を閉じる\nCtrl+C アプリ終了"
        ))
        .block(Block::default().borders(Borders::ALL)),
        areas[1],
    );
}
