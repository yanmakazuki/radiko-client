# Changelog

このプロジェクトの主な変更です。形式は [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) に基づき、版付けは [Semantic Versioning](https://semver.org/spec/v2.0.0.html) に従います。

## [Unreleased]

## [0.1.0] - 2026-10-31

### Added

- macOS と Linux 向けの CLI と Ratatui TUI（`radiko-client`）。画面のない取得、認証、再生、保存はライブラリ `radiko`。
- 全国と地域の番組表。ジャンル、出演者分類、キーワードで絞り、名前付きの検索条件を保存する。起動時は日付だけを、日本時間の午前5時区切りの当日に戻す。
- ライブと、過去30放送日のタイムフリー。純 Rust で AAC を復号し、CPAL で出力する。選んだ枠の末尾、ライブは終了日時で止まり、次の番組へは進まない。放送前の枠は再生しない。
- 公式 AI チャプターの表示。イベント原点が放送枠の開始と一致する章だけ、その位置から再生する。
- ログインと、再利用できるセッションのローカル保存。パスワードは保存しない。
- 一時停止、音量、タイムフリーのシーク、対話操作での局と番組の切り替え。
- 再生中の HTTP 401 で、配信認証を1回やり直す。ライブが配信窓から遅れても追いつく。スリープ復帰と長い通信断は、5分まで再接続する。
- Linux では CPAL の ALSA 既定 PCM `default` を開く。要求したレートの f32、i16、i32 を受け付ける。
- MIT ライセンス。
- `v*` タグの push だけで、macOS の Apple silicon（`aarch64-apple-darwin`）向け tar.gz と SHA256 を公開する。Linux の成果物と Intel Mac 向けの成果物は含めない。
- タグの `MAJOR.MINOR.PATCH` が両方の `Cargo.toml` の `version` と違うときは、リリースを作らずに失敗する。`v0.1.0-test.1` のような SemVer のプレリリース接尾辞は、核の版が一致するときだけプレリリースとして公開する。接尾辞は `Cargo.toml` には書かない。本文はこの節から作る。

### Changed

- 終了時は、HTTP の完了を待たずに音声出力を閉じる。
- 要求したレートを含まない機器は、別のレートでは開かない。
- 確認済みの停止文は接頭辞なしで出す。バッファが空いた無音には「受信待ち…」を出す。
- ダイアログとチャプターの `q` は閉じるだけにする。ログアウト確認の `q` はログアウトしない。
- 機器がない Linux では、列挙と出力を開くあいだだけ、JACK などの標準エラーを隠す。

### Fixed

- ライブの失効した chunklist は、最初の配信 URL から取り直す。
- タイムフリーで続きが配信窓から外れたときは、プレイリスト不正と分けて止める。
- 標準出力が閉じているときは終了コード 141 で戻る。閉じた標準エラーでは表示を捨て、終了コードは変えない。
- 親が SIGINT を無視していても、検証スクリプトは中断する。

[Unreleased]: https://github.com/yanmakazuki/radiko-client/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/yanmakazuki/radiko-client/releases/tag/v0.1.0
