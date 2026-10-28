//! 標準エラーへの表示。
//!
//! 書けないときは文字を捨てる。閉じたパイプでも、それ以外の書き込みエラーでも、
//! 呼び出し側の終了コードは変えない。SIGPIPE を既定へ戻す `unsafe` は使わない。

use std::fmt::Arguments;
use std::io::{self, Write};

fn discard(result: io::Result<()>) {
    let _ = result.ok();
}

/// 改行を足さずに書く。ログインのプロンプトと同じである。
pub fn write(args: Arguments<'_>) {
    discard(write!(io::stderr(), "{args}"));
}

/// 一行を書く。開けているときのバイト列は、行末の改行まで含めて同じである。
pub fn write_line(args: Arguments<'_>) {
    discard(writeln!(io::stderr(), "{args}"));
}

/// プロンプトをすぐに見せる。失敗しても終了コードは変えない。
pub fn flush() {
    discard(io::stderr().flush());
}
