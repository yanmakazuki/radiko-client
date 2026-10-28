//! 標準エラーの進捗と失敗。
//!
//! 書き込みは [`radiko::stderr`] の一つに集める。読めない、閉じている、書けない、
//! のいずれでもパニックせず、終了コードも変えない。

macro_rules! stderr_ln {
    ($($arg:tt)*) => {
        radiko::stderr::write_line(format_args!($($arg)*))
    };
}

macro_rules! stderr_write {
    ($($arg:tt)*) => {
        radiko::stderr::write(format_args!($($arg)*))
    };
}

pub(crate) use stderr_ln;
pub(crate) use stderr_write;
