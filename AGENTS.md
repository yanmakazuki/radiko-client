# Repository Guidelines

## Scope and Architecture

This macOS and Linux Rust client provides filtered station and regional/nationwide date-range listings, authentication, HLS probes, bounded audio output, saved search CLI, and a search/live/time-free TUI. Chapter jumps require a matching event origin and slot; broader real-device validation remains unfinished. Read [design.md](docs/design.md) and [architecture.md](docs/architecture.md) before implementation; update both when requirements change. Record dated verification evidence and limitations in [implementation.md](docs/implementation.md). Preserve pure Rust decoding; do not substitute external players or OS decoders automatically.

## Project Structure

The workspace has two crates. `crates/radiko` (`radiko`) is the screen-free library: domain, catalog selection, `api` HTTP/parsing, player, storage, session, and app services. `crates/radiko-client` is the CLI and Ratatui TUI and depends on `radiko`. Shared synthetic assets stay in `tests/fixtures/`.

- `crates/radiko/src/app.rs` and `crates/radiko/src/app/search.rs`: main-loop-owned search state, editing, request generations, and bounded cancellable acquisition; `crates/radiko-client/src/tui.rs`: Ratatui search/playback frontend and terminal restoration.
- `crates/radiko/src/app/playback.rs`: bounded TUI playback commands, session resolution, snapshots, and output shutdown.
- `crates/radiko/src/app/chapters.rs`: chapter request generations, bounded acquisition, and origin/slot-bound jump validation; listening permission is rechecked by playback.
- `crates/radiko/src/app/auth.rs`: asynchronous session checks, explicit login/logout jobs, and contract-only snapshots; credential drafts are zeroized and never logged.
- `crates/radiko-client/src/tui/filters.rs`: local date/scope/classification/keyword editing, validation, and observed-classification display; no I/O.
- `crates/radiko-client/src/tui/viewport.rs`: global list/page selection and visible row ranges; format only visible rows. `crates/radiko-client/src/tui/failures.rs`: read-only current acquisition failures and malformed-slot provenance. `crates/radiko-client/src/tui/scroll.rs`: bounded wrap-aware help/chapter-summary scrolling. `crates/radiko-client/src/tui/help.rs`: read-only key guide.
- `crates/radiko/src/app/named.rs` and `crates/radiko-client/src/tui/named.rs`: serialized local saved-search operations and browser; recalls populate drafts without fetching.

- `crates/radiko/src/domain/search.rs`: validated criteria and startup date reset; `crates/radiko/src/storage/searches.rs`: atomic versioned criteria storage; `crates/radiko-client/src/searches.rs`: offline named-search management CLI.

- `crates/radiko/src/catalog/search.rs`: pure AND/OR filtering and observed classification aggregation.

- `crates/radiko-client/src/chapters.rs`: read-only chapter CLI; `crates/radiko/src/api/stream/chapters.rs`: metadata identity checks and bounded chapter acquisition.

- `crates/radiko/src/api/http/diagnostics.rs`: opt-in bounded request-stage timings for active HLS workers; no URLs, headers, bodies, or audio callback work.
- `crates/radiko/src/player/state.rs`: owned playback snapshots and active playback states; `crates/radiko/src/player/diagnostics.rs`: bounded worker-stage wall-clock measurements, independent of the audio callback.
- `crates/radiko-client/src/play.rs`: continuous playback, switching, and seek CLI; `crates/radiko/src/player/seek.rs`: latest-request playback preparation; `crates/radiko/src/player/playback.rs`: bounded worker and output control; `crates/radiko/src/api/stream/playback.rs`: stateful HLS acquisition.
- `crates/radiko-client/src/programs.rs`: program listing CLI; `crates/radiko/src/catalog.rs`: region selection and bounded acquisition; `crates/radiko/src/api/catalog.rs`: uncached metadata HTTP/XML.
- `crates/radiko-client/src/main.rs`: CLI arguments and reporting; `crates/radiko/src/lib.rs`: library exports and `prelude`.
- `crates/radiko/src/domain.rs`: bounded broadcast-day ranges, slot identity, status, and chapters.
- `crates/radiko/src/api/`: authentication, bounded HTTP, and stream probes; `crates/radiko/src/api.rs`: chapter parsing.
- `crates/radiko/src/player.rs`, `crates/radiko/src/player/aac.rs`, and `crates/radiko/src/player/output.rs`: decoding probes, stateful AAC/SBR decoding, and bounded CPAL output; `crates/radiko/src/session.rs` and `crates/radiko/src/storage.rs`: session types, `DataDir`, and persistence.
- `crates/radiko/tests/` and `crates/radiko-client/tests/`: library and CLI integration tests; `tests/fixtures/`: synthetic JSON, XML, and HLS assets.

## Build and Development Commands

```sh
cargo build --workspace
cargo run -- --help
cargo run -- inspect-chapters tests/fixtures/chapters.json 90 0
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

These build, inspect CLI behavior, check formatting, reject lint warnings, and run tests. For the documented macOS SDK linker failure, prefix Cargo commands with `SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk`. On Linux, install pkg-config and the ALSA development files (`libasound2-dev` on Debian and Ubuntu, `alsa-lib-devel` on Fedora) before building. Maintain `Cargo.lock`. The Checks workflow runs on macos-15 and ubuntu-24.04: stable formatting, lints, and Rust/Python tests, plus Rust 1.85.0 tests. Use `--locked` in CI and keep live probes out of automatic jobs. Linux jobs install `libasound2-dev` and `pkg-config` first. A separate workflow runs the ignored `native_muted` output tests against a PulseAudio null sink only for pull requests that change the audio path.

## Coding Style and Domain Rules

Use Rust 2024, four-space indentation, and rustfmt. Use `snake_case` functions/modules and `UpperCamelCase` types. Return errors for routine failures instead of `unwrap()` or `expect()`. Keep communication, domain logic, storage, and playback responsibilities separate. Identify slots by station ID plus start time; broadcast days begin at 05:00 JST. Keep broadcast status distinct from listening permission.

## Testing Guidelines

Use Rust's built-in tests with descriptive behavior-based names. Ordinary tests must use fixtures, mocks, and generated audio without accounts or radiko access. Cover boundaries, malformed responses, permissions, and failure paths; no numerical coverage threshold is configured. Run the three checks above before review. Separate explicit network probes from automated tests; acquisition success does not establish playback support.

## Commits and Pull Requests

History uses short Japanese descriptions and an English initial commit, without an established prefix convention. Write concise commits describing the change. PRs should explain behavior, relevant issues, validation commands/results, and remaining limitations.

## Security and Configuration

Never persist passwords or log credentials, cookies, tokens, or signed URLs. Use `--data-dir` for session and search-condition storage: atomic writes, directories `0700`, files `0600`. Preserve corrupt files until explicit successful login. Do not cache program data or persist audio.

---

## プロジェクトの目的

macOSとLinux向けのRust製radiko TUIクライアントを開発する。全国の番組をジャンル・出演者分類・キーワードで検索し、ライブと過去30日のタイムフリーを再生できることを目指す。Linuxの再生は実機未確認である。

実装前に[設計書](docs/design.md)と[アーキテクチャ設計](docs/architecture.md)を読む。仕様の判断は設計書とユーザーの最新の指示に従い、仕様変更時は設計書とアーキテクチャ設計も整合させる。

## 現在の状態

2026年10月26日時点の要約は [docs/validation.md](docs/validation.md)、日付とビルドごとの記録は [docs/implementation.md](docs/implementation.md) にある。仕様は [docs/design.md](docs/design.md)、構成は [docs/architecture.md](docs/architecture.md) に合わせる。

実装を変えるときに維持すること:

- 実配信で確認した連続再生は ADTS の AAC/SBR である。単一 AAC トラックの fragmented MP4 は、生成音声と、ADTS をメモリ内で詰め直した照合まで接続している。実配信の非 ADTS が見つかるまで、Range、複数 run、編集リスト、PS 再構成は足さない。
- イベント原点が枠の開始と違う実例は確認していない。補正は足さない。
- 既定出力は、同じ名前の列挙デバイスを開く。Linux では CPAL の ALSA 既定名 `default` を開く。自動再開、任意の機器選択、再生中の出力先追跡はしない。
- 録音、音声の永続保存、外部プレイヤーや OS デコーダーへの自動切り替えはしない。
- 検証を書いたら、確認日、対象、手順、結果を implementation.md に足し、validation.md の要約も合わせる。取得できたことだけで再生対応済みにしない。

## 開発の順序

次の四段は接続済みである。残る確認は [docs/validation.md](docs/validation.md) の「まだ確認していないこと」に従う。

1. 認証、ライブ、通常タイムフリー、8〜30日前の配信。
2. 実配信のコンテナとコーデック、純Rustのデコードと連続再生。
3. 全国の番組表、検索、検索条件の保存、公式AIチャプターと放送枠の対応。
4. チャプターのジャンプを含むTUI、操作、終了処理。

再生方式やAPIを推測で固定しない。技術的に成立しない条件が見つかったら、再現手順と根拠を記録して報告する。外部プレイヤーやOSのデコードAPIへ自動的に方針変更しない。

## 構成と実装ルール

- ワークスペースはライブラリクレート `radiko`（`crates/radiko`）と、CLI・TUI のバイナリ `radiko-client`（`crates/radiko-client`）に分ける。ライブラリは `api`、`catalog`、`player`、`app`、`domain`、`storage` の責務を分離し、画面の依存を持たない。画面のコードは `crates/radiko-client/src/tui.rs` と `crates/radiko-client/src/tui/` にある。アプリ状態は単一イベントループで更新し、処理領域ごとの要求ID・世代で古い結果を破棄する。
- 採用技術は設計書に従う。依存ライブラリの対応形式・API・featureは公式資料と実際の採用バージョンで確認する。
- アプリケーション用の`Cargo.lock`を管理する。
- 画面から通信・デコード処理を直接実行せず、コマンドと状態通知を介して連携する。
- ネットワーク待機やデコードでTUIを止めない。CPALの音声コールバックでは通信・デコード・待機を行わない。
- 再生バッファには容量上限を設ける。局切替・シークでは旧タスクをキャンセルし、旧音声が新しい再生に混ざらないようにする。
- 終了・エラー時は音声出力を停止し、raw modeと代替画面を解除してターミナルを復元する。
- 通常運用で発生する通信・認証・解析エラーを`unwrap()`や`expect()`で処理しない。

## 番組データと検索

- 起動時の初期画面は毎回タイムフリーとし、日付は保存済みの条件にかかわらず当日に戻す。「当日」は日本時間の午前5時区切りのradikoの放送日とし、午前0時から5時未満は前日の放送日とする。地域・ジャンル・出演者分類・キーワードのみ前回の条件を復元し、初回は全国・分類指定なし・キーワードなしとする。
- 当日分の放送前番組も表示する。日本時間の現在日時と開始・終了日時から「放送前」「放送中」「放送終了」を文字ラベルで示し、時間の経過に合わせて通信なしで更新する。
- 放送前の番組は詳細閲覧のみ可能とし、再生操作時は理由を表示して配信取得を行わず、現在の再生を継続する。放送状態と聴取可否は別に管理し、契約・配信対象・聴取期限に応じた可否と理由を表示する。未確認の場合は「再生可否未確認」とし、放送終了だけで再生可能と判断しない。
- 放送枠の識別には放送局IDと開始日時を使う。番組IDのみで分割枠を統合しない。
- 日本時間を明示し、午前5時を放送日の境界として扱う。
- `genre/program`と`genre/personality`を別々に保持する。「女性アイドル」は出演者分類である。
- 分類ID・名称は取得データから集計する。観測済みの分類一覧を全分類とみなさない。
- 分類が空でも番組を捨てず、「未分類」として検索・表示できるようにする。
- 絞り込み条件は日付、地域、ジャンル（番組ジャンル）、出演者分類、キーワードの5項目とする。日付は単日または期間で指定する。異なる条件間はAND、同じ分類内の複数選択はORとする。
- 入力中と確定済みの検索条件を分ける。入力・選択変更だけでは通信せず、検索条件画面の「検索」操作で確定したときに取得する。同画面の`Enter`は検索確定、番組一覧の`Enter`は再生とする。編集キャンセル時は確定済みの条件を維持する。
- 条件編集・検索確定・取得失敗の間も現在の再生を続ける。検索取得と再生用通信を独立させ、新しい検索確定時は古い検索だけをキャンセルする。古い検索結果を新しい一覧へ反映しない。
- 地域は「その地域で聴ける局」と定義し、地域別の局一覧から対象局を決定する。複数地域に含まれる同じ局は局ID、同じ放送枠は「放送局ID＋開始日時」で重複排除する。別局の同名番組や開始日時の異なる分割枠は統合しない。
- 全国の局IDを重複排除し、選択した期間のみを最大4並列で取得する。全国30日分を起動時に一括取得しない。
- 番組表・局一覧・分類一覧のキャッシュは使用しない。起動時、検索条件の確定時、手動更新時に必要なデータを取得する。現在の検索・表示用データのみメモリに保持し、次の取得を省略するために再利用しない。
- 取得途中・部分失敗・未取得の範囲を画面で明示する。取得失敗を前回のデータで補完しない。
- 検索条件を保存する。起動時の自動復元では日付を当日に戻し、他の検索条件のみ復元する。名前付き条件の明示的な呼び出しでは保存した日付・期間も適用する。再生位置は初期版では保存しない。
- 番組終了時は再生を停止し、次の番組へ自動で移らない。ライブは選択した番組の終了日時で停止して残った音声バッファを破棄し、タイムフリーは選択した放送枠の末尾で停止する。画面に「番組終了」と表示する。

## チャプター

- タイムフリーの公式AIチャプターを取得・表示し、選択した開始位置から再生する。独自の文字起こし・AI生成は行わない。
- 取得先は `https://api.annex-cf.radiko.jp/v1/programs/{eventId}/chapters` とする。根拠は公開実装と、このプロジェクトでの取得結果である。公式が保証した外部API仕様としては扱わない。番組表XMLの番組IDと `eventId` を同一視しない。
- 詳細の`c`で一覧を開くときに対象番組分だけ取得する。一括取得・永続キャッシュは行わず、開き直し・手動更新で再取得する。取得中・未提供・失敗・対応付け未確認を区別する。成功した空配列は生成待ちの可能性もあるため「チャプター未提供」と表示し、古い応答を別番組へ反映しない。
- チャプターの開始・終了位置を検証し、放送枠外など不正な位置へジャンプしない。取得中や失敗時も現在の再生を続ける。
- ジャンプ再生は通常の聴取可否判定とシーク処理を使用する。再生不可なら現在の再生を継続し、理由を表示する。チャプターの存在だけで再生可能と判断しない。
- 詳細で選択中の番組と再生中の番組を、番組名・放送局・放送日時で明示する。閲覧・選択移動のみでは再生を切り替えない。チャプターの`Enter`確定で、同じ放送枠ならシーク、別の放送枠なら対象番組へ切り替えて指定位置から再生する。一致判定には「放送局ID＋開始日時」を使う。

- 楽曲用の別APIは追加せず、`isMusic=true`を除外する。分割枠への時刻補正、現在チャプターの重複・隙間判定はアーキテクチャ設計に従う。
- 再生準備中は現在の再生を継続し、準備成功後に切替を確定する。ライブからのチャプター確定はタイムフリーへの切替とする。

## 認証と機密情報

- パスワードをコード、設定ファイル、ログ、テストデータへ保存しない。
- radikoのパスワードは永続保存しない。再利用可能なセッション情報のみ、OSに依存しないバージョン付きJSONファイルに平文で保存する。KeychainなどOS固有の認証情報ストア、暗号化、保存用パスフレーズは使用しない。
- 保存先は`--data-dir`で指定可能にし、既定では`directories`のユーザー用データディレクトリ内の`session.json`を使う。起動時に自動で読み込み、有効性を確認する。セッション保存は番組データのキャッシュ禁止とは別の扱いとする。
- ファイルなしの場合は未ログインで起動する。読み込み失敗・破損・未対応形式では理由を表示し、既存ファイルを自動で上書きせず、未ログインで番組探索を継続する。明示的な再ログインが成功した場合は新しいセッションを保存する。
- 原子的に保存し、一時ファイルを含めUnixではディレクトリ`0700`・ファイル`0600`にする。ログアウト時はメモリと保存ファイルからセッション情報を削除する。
- Cookie、認証トークン、署名付き配信URLをログや検証資料に含めない。HTTPのデバッグ出力も同様に伏せる。
- ログイン成功と聴取権限を区別する。契約状態・番組の配信条件・サーバー応答を確認する。
- セッション失効時は再ログインを案内する。通信断の再試行は最大3回とする。
- 実アカウントの認証情報はユーザーがローカルで入力できるようにし、チャットへの貼り付けを求めない。

## 検証

Cargoプロジェクト作成後は、変更に適したテストと次のチェックを実行する。

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

- 通常の自動テストは、実アカウントやradikoへの通信に依存させない。XML・HLSのfixtureとモックを使う。
- fixtureには認証情報を含めない。音声テストには生成した音声または利用可能なサンプルを使う。
- XMLの空分類・未知の分類・分割枠・日付境界、検索条件の組み合わせと保存・復元を検証する。
- 地域別の聴取可能局の選択と、複数地域に含まれる同じ局・放送枠の重複排除を検証する。別局の同名番組と開始日時の異なる分割枠が残ることを確認する。
- 起動・検索条件確定・手動更新で再取得されることと、キャッシュを使用しないことを検証する。
- 条件入力・選択変更・編集キャンセルでは通信が発生せず、確定時に取得されることを検証する。検索中・検索失敗時の再生継続と、古い検索結果が新しい結果を上書きしないことを確認する。
- 当日の放送前番組の表示、開始・終了日時の境界での状態更新、放送前番組の再生操作時に配信取得や現在の再生停止が発生しないことを検証する。配信対象外や可否未確認を再生可能と誤表示しないことを確認する。
- 午前4時59分と午前5時の起動日付、保存済みの日付・期間が当日に戻ること、他の検索条件のみが復元されることを検証する。
- ライブの終了日時とタイムフリーの放送枠末尾で停止し、次番組へ自動で移らず残った音声が出力されないことを検証する。
- 認証失敗・失効・契約不足・配信対象外・全国取得の部分失敗を区別できることを確認する。
- セッションの自動読み込み・ファイルなし・破損・未対応形式・失効・保存中断・ファイル権限・ログアウト時の削除・保存先指定を検証する。OS固有の認証情報ストアにテストを依存させない。
- 音声のセグメント境界・形式変更・バッファ不足・シーク・局切替・出力機器切断・終了処理を検証する。
- チャプターJSONのfixtureとモックで空配列・不正時刻・タイトルの優先順位・並べ替え・取得失敗・古い応答の破棄を検証する。取得中の再生継続、ジャンプ時の聴取可否判定・旧音声破棄・放送枠末尾での停止を確認する。
- 選択中と再生中の番組表示、チャプターの閲覧・選択移動時の再生継続、確定時の同一放送枠へのシークと別放送枠への切替を検証する。同名番組や分割枠を同一視しないことを確認する。
- チャプターの実通信検証では`eventId`と放送枠の対応、認証条件、7日以内と8〜30日前の提供範囲、開始秒数と実音声の位置を確認する。公開コードの確認と実通信・再生の検証結果を分けて記録する。
- 実通信テストは明示的に実行する方式に分ける。地元とエリア外のライブ、7日以内と8〜30日前の対象番組で確認する。
- 完成条件は、実配信を30分連続再生しながら検索・キー操作が応答すること。
- 環境や契約の制約で未実行の検証は、実行済みの検証と分けて報告する。

## 初期版の範囲

録音、音声の永続保存、ポッドキャスト、通知、再生位置保存、macOS と Linux 以外の OS 対応は追加しない。ユーザーが範囲を変更した場合は、その指示を反映して設計書を更新する。
