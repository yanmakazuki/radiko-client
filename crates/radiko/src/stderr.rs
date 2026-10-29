//! 標準エラーへの表示。
//!
//! 書けないときは文字を捨てる。閉じたパイプでも、それ以外の書き込みエラーでも、
//! 呼び出し側の終了コードは変えない。SIGPIPE を既定へ戻す `unsafe` は使わない。
//!
//! Linux では、アプリの表示は fd 2 そのものではなく、最初に複製した標準エラーへ書く。
//! 機器の列挙と出力を開くあいだは fd 2 を `/dev/null` に差し替えるので、JACK などの
//! プラグインが fd 2 へ書いても、こちらの表示は残る。

use std::fmt::Arguments;
use std::io::{self, Write};

#[cfg(target_os = "linux")]
#[path = "stderr_fd.rs"]
mod fd;

fn discard(result: io::Result<()>) {
    let _ = result.ok();
}

/// `RADIKO_ALSA_LOG=1` 以外では、ALSA と JACK などのプラグインログを出さない。
pub(crate) fn native_logs_suppressed(value: Option<&str>) -> bool {
    value != Some("1")
}

/// `RADIKO_ALSA_LOG=1` のとき、ALSA と JACK などのプラグインログをそのまま出す。
pub(crate) fn show_native_audio_logs() -> bool {
    !native_logs_suppressed(std::env::var("RADIKO_ALSA_LOG").ok().as_deref())
}

/// 改行を足さずに書く。ログインのプロンプトと同じである。
pub fn write(args: Arguments<'_>) {
    if cfg!(target_os = "linux") {
        #[cfg(target_os = "linux")]
        fd::write_fmt(args, false);
    } else {
        discard(write!(io::stderr(), "{args}"));
    }
}

/// 一行を書く。開けているときのバイト列は、行末の改行まで含めて同じである。
pub fn write_line(args: Arguments<'_>) {
    if cfg!(target_os = "linux") {
        #[cfg(target_os = "linux")]
        fd::write_fmt(args, true);
    } else {
        discard(writeln!(io::stderr(), "{args}"));
    }
}

/// プロンプトをすぐに見せる。失敗しても終了コードは変えない。
pub fn flush() {
    // Linux の表示はバッファしない。std の標準エラーに残っている分だけ流す。
    discard(io::stderr().flush());
}

/// ネイティブライブラリが fd 2 へ書くあいだ、それを `/dev/null` に逃がす。
///
/// ドロップで必ず戻す。Linux 以外では何もしない。`RADIKO_ALSA_LOG=1` のときも
/// 差し替えない。アプリの `stderr::write` は複製した元の標準エラーへ書くので、
/// このガードのあいだも失わない。
#[must_use = "dropping this immediately restores stderr"]
pub(crate) struct NativeStderrSilence {
    #[cfg(target_os = "linux")]
    armed: bool,
}

pub(crate) fn silence_native_stderr() -> NativeStderrSilence {
    #[cfg(target_os = "linux")]
    {
        if show_native_audio_logs() {
            return NativeStderrSilence { armed: false };
        }
        NativeStderrSilence {
            armed: fd::begin_redirect(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        NativeStderrSilence {}
    }
}

impl Drop for NativeStderrSilence {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if self.armed {
            fd::end_redirect();
        }
    }
}
