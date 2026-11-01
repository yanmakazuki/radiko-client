# radiko-client

macOS と Linux のターミナルで使う radiko クライアントです。全国・地域・局の番組を、最大30放送日の範囲でジャンル・出演者分類・キーワードから探し、ライブとタイムフリーを再生します。音声は純 Rust でデコードし、CPAL で出力します。Linux では、yanma の QEMU 仮想マシンで CLI の `play-live` を90分再生したことまで確認しています。物理 PC での TUI 再生と、同じ仮想マシンでの TUI 再生は未確認です。確認の範囲は [docs/validation.md](docs/validation.md) の「Linux の実機検証」です。

| 知りたいこと | 文書 |
| --- | --- |
| 起動のしかたとキー | この README |
| いまの動きの仕様 | [docs/design.md](docs/design.md) |
| コードの分かれ方と上限 | [docs/architecture.md](docs/architecture.md) |
| 実機で確認できた範囲と、その手順 | [docs/validation.md](docs/validation.md) |
| 日付・ビルドごとの検証記録 | [docs/implementation.md](docs/implementation.md) |
| 版ごとの変更 | [CHANGELOG.md](CHANGELOG.md) |

## 必要なもの

- macOS または Linux
- Rust 1.85 以上。依存は `Cargo.lock` に固定する
- Linux では C コンパイラ、pkg-config、libasound の開発ファイル。Debian 13 と Ubuntu 24.04 は `build-essential`、`pkg-config`、`libasound2-dev`。Fedora は `gcc`、`pkgconf-pkg-config`、`alsa-lib-devel`。Ubuntu 24.04 でソースからビルドする手順は「Ubuntu 24.04 でビルドして output-info を実行する」
- 再生には、公式 Web にログインできるダブルプランの保存セッション
- 番組表の閲覧はログイン不要

macOS では、既定の MacOSX27.0 SDK でリンクに失敗することがあります。そのときは Cargo の前に次を付けます。

```sh
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
```

## インストール

公開するバイナリは、macOS の Apple silicon（`aarch64-apple-darwin`）だけです。Intel Mac（`x86_64-apple-darwin`）向けのファイルは出していません。Intel Mac は、下の「ソースからビルドする」で作ります。

Linux のバイナリも、このリリースにはありません。Linux はソースからビルドします。Linux の成果物は、物理 PC で再生を確認したあとに出します。

### リリースから入れる

安定版のタグは `v` に、`crates/radiko` と `crates/radiko-client` の `Cargo.toml` の `version` を続けたものです。どちらもいまは `0.1.0` なので、タグは `v0.1.0` です。二つの `version` が違うとき、またはタグの `MAJOR.MINOR.PATCH` がそれと違うとき（クレートが `0.1.0` なのに `v0.2.0` など）は、ワークフローがリリースを作る前に失敗します。

`v0.1.0-test.1` のように、核の版のあとに SemVer のプレリリース接尾辞を付けたタグは、核が Cargo.toml と一致すれば通ります。接尾辞は Cargo.toml には書きません。GitHub 上のリリースはプレリリースになり、Latest にはしません。本文は CHANGELOG のその核の節（`## [0.1.0]`）です。`v0.1.0+gha` のようなビルドメタデータは受け付けません。

次の例の `0.1.0` は、入れる版に置き換えます。ファイル名には版と `aarch64-apple-darwin` が入ります。バイナリには Developer ID の署名がなく、公証（notarization）もされていません（ad-hoc 署名のみです）。

`curl -fLO` で受け取ったファイルには隔離属性 `com.apple.quarantine` が付かないので、Gatekeeper は警告しません。

```sh
ver=0.1.0
base="https://github.com/yanmakazuki/radiko-client/releases/download/v${ver}"
asset="radiko-client-${ver}-aarch64-apple-darwin.tar.gz"
curl -fLO "${base}/${asset}"
curl -fLO "${base}/${asset}.sha256"
shasum -a 256 -c "${asset}.sha256"
tar xzf "${asset}"
"radiko-client-${ver}-aarch64-apple-darwin/radiko-client" --help
```

アーカイブの中は `radiko-client` と `LICENSE` です。

ブラウザで保存したファイルを展開したときは、ダウンロードと同じディレクトリで隔離属性を外します。macOS 15 以降は、「システム設定 > プライバシーとセキュリティ > このまま開く」からも開けます。

```sh
xattr -d com.apple.quarantine "radiko-client-${ver}-aarch64-apple-darwin/radiko-client"
```

### ソースからビルドする

Linux と Intel Mac は、リポジトリを取得してビルドします。Ubuntu 24.04 の手順は次の節です。依存の入れ方は「必要なもの」です。

```sh
cargo build --locked --release
```

出来たバイナリは `target/release/radiko-client` です。

## Ubuntu 24.04 でビルドして output-info を実行する

次のブロックは、クリーンな Ubuntu 24.04 で、このリポジトリのルートをカレントにして上から順に実行します。Rust は rustup の stable を入れます。1.85 以上です。root のときは `apt-get` を直接実行し、それ以外は `sudo` を付けます。

```sh
if [ "$(id -u)" -eq 0 ]; then
  apt_get() { apt-get "$@"; }
else
  apt_get() { sudo apt-get "$@"; }
fi
export DEBIAN_FRONTEND=noninteractive
apt_get update
apt_get install -y --no-install-recommends build-essential pkg-config libasound2-dev curl ca-certificates
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
. "$HOME/.cargo/env"
rustc --version
cargo build --locked
cargo run --locked --quiet -- output-info
```

`rustc --version` の版が 1.85 未満なら、ここで止めて `rustup toolchain install 1.85.0` と `rustup default 1.85.0` のあと、`. "$HOME/.cargo/env"` からやり直します。2026-10-31 にこの手順を実行したときの stable は 1.99.1 でした。

音声機器がないときの `output-info` は終了コード 1 です。標準エラーは次の1行だけです。

```text
エラー: 既定の音声出力機器がありません
```

本体の「既定の音声出力機器がありません」は `OutputError::NoDevice` の表示です。CLI は `エラー: ` を先頭に付けます。

## 音声サーバーごとの前提

Linux の再生は CPAL 0.16.0 の ALSA ホストです。開く PCM の名前は `default` です。`pipewire`、`pulse`、`jack`、`oss`、カードの `plughw` は、名前が `default` と違うので選びません。使うサーバーは一つにします。Debian 13 と Ubuntu 24.04 では、パッケージ `pipewire-alsa` が `pulseaudio` と衝突します。

確認に使う `aplay` は、Debian 13、Ubuntu 24.04、Fedora のどれもパッケージ `alsa-utils` です。

```sh
aplay -L
```

出力に PCM 名 `default` があることを見ます。説明の文は環境で違います。そのあと、ビルド済みのリポジトリルートで次を実行します。

```sh
cargo run --locked --quiet -- output-info
cargo run --locked --quiet -- probe-output --mute 3
```

`output-info` は音を出さず、既定機器が 48000Hz（引数で 8000〜192000Hz）の mono または stereo の f32、i16、i32 のどれを宣言しているかを読みます。f32 を優先します。対応しているときの2行目は、`48000Hz / f32 / 2ch: 対応宣言あり（再生時に再確認）` のように、ソースの「対応宣言あり（再生時に再確認）」で終わります。`probe-output --mute` は通信も復号もせず、無音で機器だけを開きます。秒数は 1〜1800 で、省略時は 30 秒です。上の `3` は短い確認です。完了時の標準出力は「無音出力完了: 」で始まります。

パッケージ名は、Ubuntu 24.04 は packages.ubuntu.com の noble、Debian 13 は trixie のパッケージ一覧、Fedora は packages.fedoraproject.org で存在を確認しています。

### PipeWire

ALSA から PipeWire へ出すプラグインは、三つのディストリビューションともパッケージ `pipewire-alsa` です。依存で `pipewire` が入り、`default` を PipeWire の PCM にします。デスクトップでは、そのセッションの PipeWire が動いている必要があります。サーバーの起動はこのクライアントの外です。

yanma の QEMU 仮想マシンの PipeWire では、`output-info`、`probe-output 1`、CLI の `play-live` 90分までを確認しています。Ubuntu 24.04 の物理 PC の PipeWire は未確認です。

### PulseAudio

ALSA の pulse プラグインで `default` を PulseAudio に繋ぎます。

| ディストリビューション | パッケージ |
| --- | --- |
| Debian 13、Ubuntu 24.04 | `pulseaudio` と `libasound2-plugins` |
| Fedora | `pulseaudio` と `alsa-plugins-pulseaudio` |

`libasound2-plugins` と `alsa-plugins-pulseaudio` が、PCM 名 `pulse` 用のプラグインと、`default` をそこへ向ける設定を入れます。Debian 13 のコンテナでは、PulseAudio 17.0 の null sink にこのプラグイン経由の `default` を繋ぎ、`output-info` と `probe-output --mute 3` を確認しています。実機ではありません。PulseAudio だけの物理 PC は未確認です。

### 素の ALSA

サウンドサーバーを使わないときは、`pipewire-alsa` も PulseAudio 用のプラグインも入れません。`default` はカード向けの定義です。カードがないと開けません。

| ディストリビューション | ランタイム |
| --- | --- |
| Debian 13、Ubuntu 24.04 | `libasound2t64`（`libasound2-dev` が依存します） |
| Fedora | `alsa-lib` |

yanma の QEMU 仮想マシンでは、`hw:0,0` を既定にした `output-info` と `probe-output 1` を確認しています。その `play-live` は未確認です。i16 だけの機器を開いた再生は、その1秒の `probe-output` 以外は未確認です。

## 音声のトラブル

失敗の日本語はソースの表示そのままで、CLI は標準エラーの先頭に `エラー: ` を付けます。

### 出力機器がない

`default` を開けないとき、Linux は `OutputError::NoDevice` です。表示は「既定の音声出力機器がありません」です。`output-info` も `probe-output` も終了コード 1 で、標準エラーはその1行です。`aplay -L` に `default` が無い、またはあってもサーバーが止まっている、カードが無い、という状態です。上の節の、使っているサーバーのパッケージを入れてから、もう一度 `output-info` を実行します。カードもサーバーも無い環境では、この1行が正しい結果です。

### レートまたは形式が合わない

このプロセスはレートを変換しません。再生の PCM は 48000Hz です。機器の宣言に、そのレートの mono または stereo の f32、i16、i32 が無いとき、`OutputError::IncompatibleOutput` の表示は次の形です。48000Hz を要求し、機器が 44100Hz・f32・2ch だけを宣言した例です。

```text
必要な形式は48000Hzのmonoまたはstereoのf32・i16・i32です。機器の対応範囲: 44100Hz・f32・2ch
```

対応範囲の並びは、機器が宣言したレート、サンプル形式、チャンネル数です。`output-info` は、機器自体はあるがレートが範囲に入らないとき、終了コード 0 で機器名の次の行にこの文を出します。再生と `probe-output` は、同じ文を `エラー: ` 付きで出して終了コード 1 です。別のレートでは開きません。変換は ALSA プラグインやサウンドサーバーの側です。

再生中に、開いた音声とサンプルレートまたはチャンネル数が違ったときは `OutputError::UnsupportedFormat` です。表示は「出力中のサンプルレートまたはチャンネル数が、開いた音声と一致しません」です。

### TUI で出力が止まったとき

CLI の表示は上のままです。TUI は、止まったあとのプレイヤー欄に対処を足します。自動では再生し直しません。次に再生するか Esc まで残します。

| 種類 | プレイヤー欄 |
| --- | --- |
| `NoDevice` | 出力先がありません。OSのサウンド設定を確認し、Enterで再生し直してください |
| `UnsupportedFormat` / `IncompatibleOutput` | 対応形式外です。OSの出力設定を確認し、Enterで再生し直してください |
| `Device` / `DeviceInfo` | 出力機器を確認してください。OSの出力先を見て、Enterで再生し直してください |
| `Stalled` | スリープ復帰・機器変更後は出力先を確認し、Enterで再生し直してください |

どの種類も次の行は「自動再開はしません」です。認証期限切れと再接続上限で止まったときの「停止中：」が同じ更新にあるときは、そちらを出します。

### エリア外

認証エリアの外の局で、会員情報のエリアフリーが有効でないとき、取得は止まります。CLI の表示は「対象の契約を確認できません（エリアフリーまたはタイムフリー30が必要です）」です。タイムフリーで、放送終了の翌放送日午前5時から7日を過ぎ、タイムフリー30が有効でないときも、CLI は同じ文言です。CLI は `エラー: ` を付けます。

TUI は、現在地のエリア（配信認証の auth2 応答の先頭にあるエリア ID）の局一覧にその局が無く、エリアフリーが未加入のときだけ「この局は現在地のエリア外です（エリアフリー未加入）」と出します。エリアフリーが未確認のときと、局一覧を見る前にタイムフリー30の不足で止まったときは、CLI と同じ契約の文です。未ログインは `I` で案内します。セッション失効、配信認証の拒否、認証期限切れの停止など、二つのキーを並べる案内は「a で再確認 / I で再ログイン」の順です。認証期限切れで止めたあとにログアウトすると、プレイヤー行は「停止中：認証期限切れ（I で再ログイン）」になります。ログアウト前は「停止中：認証期限切れ（a で再確認）」です。CLI の `login` と `session` はそのままです。

エリアフリーが有効なときのエリア外は、このエラーにはしません。`probe-live` と `probe-timefree` は「認証エリア: 」の行に「エリア外: 」を出します。配信先が、そのモードと地域のどちらにも無いときは「対象モードと地域に対応する配信先がありません」です。国内エリアを確認できないときは「国内エリアを確認できませんでした」です。

`session` はエリアフリーとタイムフリー30を「あり」「なし」「未確認」で出します。契約フラグがあることと、その番組を再生できることは別です。番組表の取得はこの判定をしません。

### ALSA と JACK のログ

起動前に環境変数を付けます。値が `1` のときだけ、ALSA のエラーハンドラも標準エラーの差し替えもせず、ALSA と JACK などのプラグインログを残します。

```sh
RADIKO_ALSA_LOG=1 cargo run --locked --quiet -- output-info
```

付けないときは、機器の列挙と出力を開くあいだのそれらのログは端末へ出さず、失敗は日本語のエラーだけです。`1` 以外の値も、ログを残しません。

## ビルド

Apple silicon の公開バイナリは「インストール」です。ここはソースの確認に使うコマンドです。

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

通常の `cargo test --workspace` はアカウントにも radiko にも繋ぎません。実配信や音声機器を使う確認は [検証範囲](docs/validation.md) に分けてあります。GitHub Actions の Checks は macos-15 と ubuntu-24.04 で、stable の fmt・clippy・Rust/Python テストと、Rust 1.85.0 の Rust テストを `--workspace --locked` で実行します。Linux のジョブは先に `libasound2-dev` と `pkg-config` を入れます。音声の経路を変えた pull request では、それとは別の ubuntu-24.04 ワークフローが PulseAudio の null sink で `output-info`、`probe-output --mute 3`、ignored の `native_muted` 2 件を実行します。

## ライブラリとして使う

画面のない処理はクレート `radiko`（`crates/radiko`）です。TUI と CLI は `crates/radiko-client` がそれを呼びます。Ratatui はライブラリの依存に入りません。

```toml
radiko = { path = "crates/radiko" }
```

よく使う型は `radiko::prelude` にあります。保存先は `radiko::storage::DataDir`、どの番組表を取るかは `radiko::catalog`、番組表 XML・認証・HLS・チャプターの通信は `radiko::api`、再生は `radiko::player`、画面向けの状態と世代は `radiko::app` です。

```rust
use chrono::TimeZone;
use radiko::domain::search::{Scope, SearchCriteria};

let jst = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
let now = jst.with_ymd_and_hms(2026, 10, 25, 4, 30, 0).unwrap();
let criteria = SearchCriteria::initial(now).unwrap();
assert_eq!(criteria.range().start().to_string(), "2026-10-24");
assert!(matches!(criteria.scope(), Scope::Nationwide));
```

## TUI を起動する

```sh
cargo run -- tui
```

引数を付けないと、前回の地域・分類・キーワードを復元し、日付だけを日本時間の午前5時区切りの当日1日に戻します。午前0時から5時未満は前日の放送日です。初回は当日・全国・分類もキーワードもなしです。最初に開く画面は毎回タイムフリーです。

```sh
cargo run -- tui --areas JP44
cargo run -- tui TBS 20261015
```

`tui` の後ろには、下の `programs` と同じ局・地域・期間・絞り込みを渡せます。

終了は、画面内ヘルプと同じ「通常画面のq、またはCtrl+Cで終了」です。通常画面は番組一覧とライブ一覧です。そこでの Esc は、出ている通知を消すだけで終了案内は出さない。通知がなければ「終了は q（または Ctrl+C）」を出し、もう一度 Esc で消す。停止文・受信待ち・再接続中は案内に替えない。再生・一時停止・選択・条件は変えません。操作一覧、取得状況、チャプター、名前付き条件では `q` と Esc は閉じるだけで、Ctrl+C は終了します。操作一覧、取得状況、名前付き条件の案内は「q / Esc 閉じる」です。ログアウト確認は「ログアウトはyで確定、n / Esc / qで取消」で、`q` はログアウトしません。Ctrl+C はそこでも終了します。キーワード、全条件、ログインの入力中は `q` が文字として入り、Esc は編集の取消、Ctrl+C は終了です。秒指定の入力中は `q` を捨て、Esc で入力を取り消し、Ctrl+C で終了します。SIGINT、SIGTERM、SIGHUP でも音声を止めて端末を戻します。SIGKILL では端末の復元も、書き込み途中の保存完了も保証しません。

### キー

| 操作 | キー |
| --- | --- |
| 操作一覧 | `?`（`q` と Esc は閉じる。Ctrl+C は終了） |
| 番組の選択 | 上下・`j`/`k`。PageUp/PageDown で1画面、Home/End で先頭と末尾 |
| 今回の取得失敗 | `e`。地域の失敗、局と放送日の失敗、不正枠の除外を見る |
| タイムフリーとライブ一覧 | `v`。切り替えるたびに取り直す |
| 再生 | Enter。ライブ一覧ではライブ、それ以外はタイムフリー |
| 放送中の枠をライブ再生 | `l` |
| 一時停止／再開、停止 | Space、`s` |
| 15秒シーク、秒指定 | 左右、`g`。どちらもタイムフリー |
| 音量 | `+` / `-` で5%ずつ。再生前でも変えられる。0%でも位置は進む |
| キーワード、全条件 | `/`、`f` |
| 名前付き条件 | `b` |
| 番組表の再取得、チャプター | `r`、`c` |
| ログイン、ログアウト、セッション再確認 | `I`、`O`、`a` |
| 通知を消す | 通常画面の Esc。出ている通知を消すだけで、終了案内は出さない。停止文・受信待ち・再接続中は案内に替えない |
| 終了案内 | 通知がないときの Esc。「終了は q（または Ctrl+C）」。もう一度 Esc で消す |
| 終了 | `q`、Ctrl+C。通常画面の Esc では終了しない |

一覧の選択や画面の切替だけでは再生は変わりません。再生欄は `[局ID / モード]` のあとに番組名を出します。放送日時は次の行です。

### 検索

`/` と `f` は、Enter で確定するまで通信しません。Esc で編集を捨て、確定済みの条件を残します。`f` の地域・番組ジャンル・出演者分類では F2 から名前を複数選べます。分類の候補は、編集を始めた時点で見えている番組だけです。

条件のあいだは AND、同じ分類の複数選択は OR です。空の分類は「未分類」として残せます。検索の途中や失敗のあいだも、いまの再生は続きます。

`b` では、確定済みの条件に名前を付けて保存します（`n`）。一覧の Enter は、保存した日付と期間も含めて編集画面へ戻すだけで、もう一度 Enter するまで取得しません。削除は `d` のあと `y` です。

ライブ一覧は、当日の取得結果のうち放送中だけを出します。開始と終了の境界は通信せずに更新します。`v` で元のタイムフリーの日付と期間へ戻り、取り直します。ライブ画面から当日以外の日付を明示して検索すると、タイムフリーへ移ります。午前5時を越えた古い一覧は `r` で更新します。

### ログインとチャプター

`I` でメールアドレスとパスワードを入れます。パスワードは表示せず、保存しません。成功したセッションだけを保存します。`O` のあと `y` で再生を止め、このクライアントの保存セッションだけを消します。`a` は保存済みセッションの有効性を再確認します。

`c` で、開いている番組の公式 AI チャプターを取ります。Tab で要約を広げ、先頭1200文字までスクロールできます。`r` で取り直し、Esc で詳細へ戻ります。イベントの音声原点が放送枠の開始と一致する章だけ、Enter でその位置から再生します。同じ枠のタイムフリーならシーク、別の枠やライブからならタイムフリーへ切り替えます。原点が違う枠の時刻補正はありません。

## ログインと保存先

```sh
cargo run -- login
cargo run -- session
cargo run -- logout
```

`login` は端末でメールアドレスとパスワードを聞きます。パスワードは残しません。ログインのあと、セッションの有効性と、エリアフリー・タイムフリー30日の契約フラグを別途確認します。契約フラグがあることと、その番組を再生できることは別です。

保存するのは再利用できるセッションだけです。既定の場所は `directories` 6 の `data_local_dir` です。

macOS では次です。

```text
~/Library/Application Support/jp.radiko-client.radiko-client/session.json
```

Linux では `$XDG_DATA_HOME/radiko-client/session.json` です。`XDG_DATA_HOME` がなければ `~/.local/share/radiko-client/session.json` です。

検索条件は同じディレクトリの `searches.json` です。`--data-dir DIR` で両方の場所を変えられます。専用のディレクトリを指定してください。Unix ではディレクトリを `0700`、ファイルを `0600` で作ります。中身は平文なので、Git には入れません。

`session` は保存ファイルを読み、サーバーで確認します。ファイルがなければ未ログインです。壊れている、形式が違う、権限が違う場合は理由を出して、ファイルはそのまま残します。`logout` は通信せず、ローカルのセッションだけを消します。公式 Web など別のログインは変わりません。

## CLI

`--help` で各コマンドの引数を見られます。日付を省略した番組表は、午前5時区切りの当日だけです。過去30日を起動時にまとめて取ることはありません。

番組表。ログイン不要で、実行のたびに取り直します。

```sh
cargo run -- programs TBS
cargo run -- programs --areas JP13,JP14 20261015
cargo run -- programs --all 20261015 --json
cargo run -- programs TBS 20261014 20261015 --keyword 生活 --json
cargo run -- programs TBS 20261015 --genre P003,P007 --personality C015
cargo run -- programs --saved 生活 --json
cargo run -- programs --restore
```

`--areas` は `JP1` から `JP47` です。その地域で聴ける局の一覧から対象を決め、局 ID で重複を除きます。期間は `YYYYMMDD` の開始日と終了日で、両端を含み最大30放送日です。取得の失敗や不正な枠があると、取れた分を出して終了コード 1 になります。JSON の `complete` も false です。

検索条件の保存は通信しません。

```sh
cargo run -- searches save 生活 TBS 20261014 20261015 --keyword 生活
cargo run -- searches list
cargo run -- searches show 生活
cargo run -- searches delete 生活
```

`programs --restore` は前回の地域・分類・キーワードを使い、日付を当日1日に戻して取り直します。名前付き条件は、保存した期間のまま使います。

再生。日時は日本時間の `YYYYMMDDHHmmss` です。選んだ枠の末尾で止まり、次の番組へは進みません。

```sh
cargo run -- play-live OBS
cargo run -- play-live TBS --seconds 30
cargo run -- play-timefree TBS 20261015120000 --offset 60 --seconds 30
cargo run -- play-timefree TBS 20261006120000 --interactive
```

`--seconds` は 1〜86400 のメディア時間の上限です。省略すると枠の末尾まで再生し、ライブは終了日時も上限にします。タイムフリーは、指定時刻を含む枠の冒頭から始まります。

`--interactive` では次を Enter で確定します。

```text
seek 120
+15
-15
pause
resume
volume 25
live OBS
timefree TBS 20261015120000 --offset 120
status
q
```

`seek` は枠の冒頭からの秒数です。`+15` と `-15` は、出力へ渡した位置からの相対移動です。音量は 0〜100 で、既定は 25% です。保存はしません。準備が失敗したあいだは、いまの再生を続けます。続けて操作したときは、最新の要求だけを採用します。`status` は追加の通信をせず、局・番組名・状態・音量・準備中の切替先を出します。

チャプターの一覧。ログイン不要で、再生はしません。

```sh
cargo run -- chapters TBS 20261015120000
cargo run -- inspect-chapters tests/fixtures/chapters.json 90 0
```

`inspect-chapters` はローカルの JSON を解析するだけです。引数はファイル、枠の秒数、原点の補正秒数です。

音を出さずに配信の先頭を見るコマンドと、出力機器だけの確認です。

```sh
cargo run -- probe-live TBS
cargo run -- probe-timefree TBS 20261014120000
cargo run -- output-info
cargo run -- output-info 24000
cargo run -- probe-output 1
cargo run -- probe-output --mute 180
cargo run -- play-sample OBS
```

`probe-live` と `probe-timefree` は音声を出さず、PCM も残しません。終了コード 0 は先頭サンプルの取得成功であり、連続再生の保証ではありません。`output-info` は、その時点の既定出力機器が指定レート（8000〜192000Hz、省略時 48000Hz）の mono/stereo の f32、i16、i32 のどれに対応すると宣言しているかを読みます。f32 を優先します。音は出さず、OS の設定も変えません。レートが合わないときは、必要なレートと機器の対応範囲を表示し、このプロセスでは変換しません。`probe-output` は生成音、`--mute` は通信も復号もせず無音で機器だけを確認します。どちらも採用した形式を表示します。Linux で ALSA や JACK プラグインの生の診断を見るときは `RADIKO_ALSA_LOG=1` を付けます。値が `1` のときだけ残ります。付けないときは、機器の列挙と出力を開くあいだのライブラリの診断を端末へ出さず、失敗は日本語のエラーにします。機器が無いときの文言と、レートが合わないときの文言は「音声のトラブル」です。`play-sample` は条件を確認したあと、先頭1セグメントだけを出して止まります。

## ライセンス

MIT License です。条件は [LICENSE](LICENSE) にあります。
