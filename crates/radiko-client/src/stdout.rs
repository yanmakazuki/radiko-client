//! CLI の標準出力。
//!
//! Rust は起動時に SIGPIPE を無視する。読み手が閉じたパイプへ `println!` で書くと、
//! `failed printing to stdout: Broken pipe` でパニックし、終了コード 101 になる。
//! ここでは書き込みの `ErrorKind::BrokenPipe` を受け取り、メッセージを足さず
//! 終了コード 141（128 + SIGPIPE）で戻す。SIGPIPE を既定動作へ戻す `unsafe` は使わない。
//! stderr の進捗と失敗は `eprintln!` / `eprint!` のままである。

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
    match writeln!(io::stdout(), "{args}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(Closed.into()),
        Err(error) => Err(error.into()),
    }
}

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
    use super::{Closed, is_closed};

    #[test]
    fn closed_stdout_is_distinct_from_other_errors() {
        let closed: Box<dyn std::error::Error> = Closed.into();
        assert!(is_closed(closed.as_ref()));
        let other: Box<dyn std::error::Error> = "引数が不正です".into();
        assert!(!is_closed(other.as_ref()));
    }
}
