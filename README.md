# radiko-client

macOS のターミナルで使う radiko クライアントです。全国・地域・局の番組を、最大30放送日の範囲でジャンル・出演者分類・キーワードから探し、ライブとタイムフリーを再生します。音声は純 Rust でデコードし、CPAL で出力します。

| 知りたいこと | 文書 |
| --- | --- |
| 起動のしかたとキー | この README |
| いまの動きの仕様 | [docs/design.md](docs/design.md) |
| コードの分かれ方と上限 | [docs/architecture.md](docs/architecture.md) |
| 実機で確認できた範囲と、その手順 | [docs/validation.md](docs/validation.md) |
| 日付・ビルドごとの検証記録 | [docs/implementation.md](docs/implementation.md) |

## 必要なもの

- macOS
- Rust 1.85 以上。依存は `Cargo.lock` に固定する
- 再生には、公式 Web にログインできるダブルプランの保存セッション
- 番組表の閲覧はログイン不要

この環境では既定の MacOSX27.0 SDK でリンクに失敗することがあります。そのときは Cargo の前に次を付けます。

```sh
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
```

## ビルド

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

通常の `cargo test --workspace` はアカウントにも radiko にも繋ぎません。実配信や音声機器を使う確認は [検証範囲](docs/validation.md) に分けてあります。GitHub Actions の Checks は macOS で、stable の fmt・clippy・Rust/Python テストと、Rust 1.85.0 の Rust テストを `--workspace --locked` で実行します。

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

終了は `q` または Ctrl+C です。入力欄を開いているときの `q` は文字として入ります。SIGINT、SIGTERM、SIGHUP でも音声を止めて端末を戻します。SIGKILL では端末の復元も、書き込み途中の保存完了も保証しません。

### キー

| 操作 | キー |
| --- | --- |
| 操作一覧 | `?`（一覧では Esc・`q` でも閉じる） |
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

保存するのは再利用できるセッションだけです。既定の場所は `directories` のユーザーデータディレクトリで、この環境では次です。

```text
~/Library/Application Support/jp.radiko-client.radiko-client/session.json
```

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

`probe-live` と `probe-timefree` は音声を出さず、PCM も残しません。終了コード 0 は先頭サンプルの取得成功であり、連続再生の保証ではありません。`output-info` は、その時点の既定出力機器が指定レート（8000〜192000Hz、省略時 48000Hz）の f32・mono/stereo に対応すると宣言しているかを読みます。音は出さず、OS の設定も変えません。`probe-output` は生成音、`--mute` は通信も復号もせず無音で機器だけを確認します。`play-sample` は条件を確認したあと、先頭1セグメントだけを出して止まります。
