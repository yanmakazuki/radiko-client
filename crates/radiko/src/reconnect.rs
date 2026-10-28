//! 再生を終えずに待つ時間。出力の作り直しと、ライブのプレイリスト再取得が共有する。
use std::time::Duration;

/// 再接続に使う実時間の上限。これを過ぎたときだけ再生を止める。
///
/// 5分は、60〜90秒の通信断に HTTP の接続10秒・要求20秒・最大3回の再試行が重なっても
/// 収まる長さである。戻らない機器や配信を、番組の終わりまで無音のままにはしない。
/// プロセスが止まっていた時間は、ここへ足さない。スリープそのもので上限に達しないようにする。
pub(crate) const LIMIT: Duration = Duration::from_secs(5 * 60);

pub(crate) const GAVE_UP: &str =
    "再接続の上限を超えたため再生を停止しました。番組を選び直して再生してください";

/// `poll` の間隔がこれより長いときは、コールバック停止ではなくプロセスの一時停止とみなす。
pub(crate) const OUTPUT_POLL_GAP: Duration = Duration::from_secs(1);

/// 定期的に `poll` されているあいだ、コールバックがこれより長く進まなければ停止とみなす。
pub(crate) const OUTPUT_CALLBACK_WAIT: Duration = Duration::from_secs(3);

/// プレイリストを取り直す間隔。失敗のたびに倍にし、`BACKOFF_MAX` で頭打ちにする。
pub(crate) const BACKOFF_START: Duration = Duration::from_secs(1);
pub(crate) const BACKOFF_MAX: Duration = Duration::from_secs(10);

/// 新しい PCM がこれ以上来ないとき、入力側の「再接続中」を出す。
///
/// 通信断からの約30秒である。バッファが空になってからの時間ではない。変えない。
pub const NOTICE_AFTER: Duration = Duration::from_secs(30);

/// 手元のバッファが空の無音がこれだけ続いたら「受信待ち…」を出す。
///
/// 機器のリングは2秒で、コールバック1回の無音は数ミリ秒から、遅れても数百ミリ秒である。
/// そのゆらぎで表示を点滅させないため、空になってから2秒続くまで出さない。
/// 2秒はセグメント境界の短い遅れより長く、`NOTICE_AFTER` より十分短い。
/// 30秒の再接続判定や、再取得の待ち、5分の上限は変えない。
pub const RECEPTION_AFTER: Duration = Duration::from_secs(2);
