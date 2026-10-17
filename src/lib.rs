//! 通信・検索・再生・保存を分離したライブラリ。CLIとTUIから共通の処理を利用する。
pub mod app;
pub mod catalog;
pub mod domain;
pub mod player;
pub mod radiko;
pub mod session;
pub mod storage;
