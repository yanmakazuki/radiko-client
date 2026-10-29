//! CLI の標準出力。
//!
//! Rust は起動時に SIGPIPE を無視する。読み手が閉じたパイプへ `println!` で書くと、
//! `failed printing to stdout: Broken pipe` でパニックし、終了コード 101 になる。
//! ここでは書き込みの `ErrorKind::BrokenPipe` を受け取り、メッセージを足さず
//! 終了コード 141（128 + SIGPIPE）で戻す。SIGPIPE を既定動作へ戻す `unsafe` は使わない。
//! 標準エラーの進捗と失敗は `stderr_ln!` に集め、書けないときは表示だけ捨てて終了コードは変えない。

use std::fmt::Arguments;
use std::io::{self, Write};

/// シェルが SIGPIPE で死んだプロセスに付けるのと同じ終了コード。
pub(crate) const BROKEN_PIPE_STATUS: u8 = 141;

#[derive(Debug)]
struct Closed;

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Broken pipe")
    }
}

impl std::error::Error for Closed {}

pub(crate) fn write_line(args: Arguments<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = io::stdout();
    match writeln!(out, "{args}") {
        Ok(()) => flush_stdout(out),
        Err(error) => Err(classify_write(error)),
    }
}

fn flush_stdout(mut out: io::Stdout) -> Result<(), Box<dyn std::error::Error>> {
    match out.flush() {
        Ok(()) => Ok(()),
        Err(error) => Err(classify_write(error)),
    }
}

fn classify_write(error: io::Error) -> Box<dyn std::error::Error> {
    if error.kind() == io::ErrorKind::BrokenPipe {
        Closed.into()
    } else {
        WriteFailed { kind: error.kind() }.into()
    }
}

/// 標準出力へ書けなかった理由。BrokenPipe は [`Closed`] のまま、終了コード 141 で戻す。
#[derive(Debug)]
struct WriteFailed {
    kind: io::ErrorKind,
}

impl std::fmt::Display for WriteFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("書き込みに失敗しました（")?;
        match self.kind {
            io::ErrorKind::StorageFull => f.write_str("空き容量不足")?,
            io::ErrorKind::PermissionDenied => f.write_str("書き込みが拒否されました")?,
            kind => write!(f, "{kind:?}")?,
        }
        f.write_str("）")
    }
}

impl std::error::Error for WriteFailed {}

pub(crate) fn is_closed(error: &(dyn std::error::Error + 'static)) -> bool {
    if error.is::<Closed>() {
        return true;
    }
    let mut source = error.source();
    while let Some(next) = source {
        if next.is::<Closed>() {
            return true;
        }
        source = next.source();
    }
    false
}

macro_rules! stdout_ln {
    ($($arg:tt)*) => {
        $crate::stdout::write_line(format_args!($($arg)*))?
    };
}

pub(crate) use stdout_ln;

#[cfg(test)]
mod tests {
    use super::{Closed, WriteFailed, is_closed};

    #[test]
    fn closed_stdout_is_distinct_from_other_errors() {
        let closed: Box<dyn std::error::Error> = Closed.into();
        assert!(is_closed(closed.as_ref()));
        let other: Box<dyn std::error::Error> = "引数が不正です".into();
        assert!(!is_closed(other.as_ref()));
    }

    #[test]
    fn stdout_write_errors_name_the_cause_and_are_not_a_closed_pipe() {
        let full = WriteFailed {
            kind: std::io::ErrorKind::StorageFull,
        };
        assert_eq!(full.to_string(), "書き込みに失敗しました（空き容量不足）");
        assert!(!is_closed(&full));
        let denied = WriteFailed {
            kind: std::io::ErrorKind::PermissionDenied,
        };
        assert_eq!(
            denied.to_string(),
            "書き込みに失敗しました（書き込みが拒否されました）"
        );
    }
}
