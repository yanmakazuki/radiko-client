# Repository Guidelines

## Scope and Architecture

This macOS Rust client currently provides filtered station and regional/nationwide date-range listings, authentication, HLS probes, bounded continuous audio output, and chapter acquisition/inspection; saved search CLI support is available; long-duration playback validation and TUI integration remain unfinished. Read [design.md](docs/design.md) and [architecture.md](docs/architecture.md) before implementation; update both when requirements change. Record dated verification evidence and limitations in [implementation.md](docs/implementation.md). Prioritize playback feasibility before search and TUI work. Preserve the pure Rust decoding requirement; do not substitute external players or OS decoders automatically.

## Project Structure

- `src/domain/search.rs`: validated criteria and startup date reset; `src/storage/searches.rs`: atomic versioned criteria storage; `src/searches.rs`: offline named-search management CLI.

- `src/catalog/search.rs`: pure AND/OR filtering and observed classification aggregation.

- `src/chapters.rs`: read-only chapter CLI; `src/radiko/stream/chapters.rs`: metadata identity checks and bounded chapter acquisition.

- `src/player/state.rs`: owned playback snapshots and active playback states.
- `src/play.rs`: continuous playback, switching, and seek CLI; `src/player/seek.rs`: latest-request playback preparation; `src/player/playback.rs`: bounded worker and output control; `src/radiko/stream/playback.rs`: stateful HLS acquisition.
- `src/programs.rs`: program listing CLI; `src/catalog.rs`: region selection and bounded acquisition; `src/radiko/catalog.rs`: uncached metadata HTTP/XML.
- `src/main.rs`: CLI arguments and reporting; `src/lib.rs`: library exports.
- `src/domain.rs`: bounded broadcast-day ranges, slot identity, status, and chapters.
- `src/radiko/`: authentication, bounded HTTP, and stream probes; `src/radiko.rs`: chapter parsing.
- `src/player.rs`, `src/player/aac.rs`, and `src/player/output.rs`: decoding probes, stateful AAC/SBR decoding, and bounded CPAL output; `src/session.rs` and `src/storage.rs`: session types and persistence.
- `tests/`: CLI, domain, and storage integration tests; `tests/fixtures/`: synthetic JSON, XML, and HLS assets.

## Build and Development Commands

```sh
cargo build
cargo run -- --help
cargo run -- inspect-chapters tests/fixtures/chapters.json 90 0
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

These build, inspect CLI behavior, check formatting, reject lint warnings, and run tests. For the documented macOS SDK linker failure, prefix Cargo commands with `SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk`. Maintain `Cargo.lock`.

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

macOS向けのRust製radiko TUIクライアントを開発する。全国の番組をジャンル・出演者分類・キーワードで検索し、ライブと過去30日のタイムフリーを再生できることを目指す。

実装前に[設計書](docs/design.md)と[アーキテクチャ設計](docs/architecture.md)を読む。仕様の判断は設計書とユーザーの最新の指示に従い、仕様変更時は設計書とアーキテクチャ設計も整合させる。

## 現在の状態

- 2026年10月16日に`searches save/list/show/delete/restore`と`programs --saved NAME`／`--restore`を追加。名前付き条件は保存期間、前回条件の復元は当日1日を適用する。version付きsearches.jsonへ原子的保存し、破損保護・権限・非待機の更新排他を実装。2日46枠→6件の保存呼び出しと当日24枠→0件の復元を確認。fmt・clippy・全135テスト成功。TUI起動時の自動復元と編集／検索世代の統合は未接続。

- 2026年10月16日に`programs ... START END`の期間取得・検索を追加（両端含む最大30放送日）。局発見は期間全体で一度、局×日付の全体で最大4並列、失敗は局・日別に保持する。TBSの2日46枠→6件、JP44の2日476枠→17件を確認。fmt・clippy・全125テスト成功。条件保存・アプリ／TUI統合は未実装。

- 2026年10月16日に`programs`へ`--keyword`／`--genre`／`--personality`、軸別未分類指定、`--classifications`を追加。同軸OR・異軸AND、分類は絞り込み前の今回の取得結果から集計する。TBS23枠→3枠、JP44の6局243枠→0件でも取得件数と観測分類を保持することを確認。fmt・clippy・全118テスト成功。期間・条件保存・アプリ／TUIでの検索は未実装。

- 2026年10月16日に`chapters STATION YYYYMMDDHHmmss`を追加。公開メタデータのeventId・開始時刻とチャプター応答の局・programIdを照合し、参考位置のみを表示する。TBS前日／10日前の分割枠で認証なしに13件／14件（楽曲除外）の取得を確認。fmt・clippy・全109テスト成功。実音声との時刻対応・ジャンプ再生は未検証／未接続。

- 2026年10月16日に番組名・状態・音量・準備先の通知と`status`を追加。`player::state::PlaybackSnapshot`は現在の対象と準備先を分離し、ライブの番組内位置は未確認として扱う。TBS前日枠でJST・番組名・停止／再開準備・音量表示を確認。fmt・clippy・全102テスト成功。アプリ／TUI統合と長時間検証は未完了。

- 2026年10月16日に対話再生の`live STATION`／`timefree STATION YYYYMMDDHHmmss [--offset SECONDS]`を追加。準備成功後のみ局・番組を切り替え、後続シークと再開も新枠を対象にする。TBS前日→10日前→OBSライブの切替・切替後シークと、OBSで120秒の出力を確認（不足0）。fmt・clippy・全96テスト成功。聴感同期、30分／TUI同時操作は未完了。

- 2026年10月16日に対話再生へ`pause`／`resume`／`volume 0〜100`を追加。タイムフリーは停止位置、ライブは同じ枠の最新位置で再開準備する。一時停止で古い準備を無効化し、音量を世代間で維持する。TBS前日枠で停止位置保持・再開・ミュート中の位置進行・q終了を確認。全91テスト成功。実配信ライブ再開・長時間停止・聴感は未検証。

- 2026年10月16日に`play-timefree --offset SECONDS`と`--interactive`を追加。`seek 秒数`／`+15`／`-15`／`q`に対応し、PDTで時刻を確認できた配信だけをシークする。旧再生を継続したままPCMと出力機器を準備し、最新要求だけに切り替える。TBS前日／10日前の開始位置、要求上書き、15秒戻る操作、q停止を確認。fmt・clippy・全87テスト成功。局切替、TUI／チャプター連携、聴感同期、30分検証は未完了。

- 2026年10月16日に`play-live STATION [--seconds N]`と`play-timefree STATION YYYYMMDDHHmmss [--seconds N]`を追加。HLSの通し番号で更新・重複・欠落を管理し、同一デコーダー／出力streamで連続再生する。PCMチャネル容量1、出力2秒。OBSライブ・TBS前日／10日前を各30秒、TBSライブのCtrl+C中断を確認。fmt・clippy・全78テスト成功。30分連続再生、聴感、実機番組終了境界、検索／TUI同時操作は未検証。

- 2026年10月16日に`probe-output [SECONDS]`と`play-sample STATION [YYYYMMDDHHmmss]`を追加。純Rustデコード済みの短いサンプルをCPALへ渡し、2秒のrtrbバッファと最大4MiBのPCM上限を設けた。生成音・地元OBS／エリア外TBSライブ・TBS前日／10日前タイムフリーの出力経路を実機で確認。fmt・clippy・全66テスト成功。通常のプローブは音声を出さない。連続再生・シーク・局切替・実際の放送終了境界・聴感品質は未検証。詳細は実装状況を参照。

- 単一局指定に加え、`programs --areas JP13,JP14 [YYYYMMDD] [--json]`と`programs --all [YYYYMMDD] [--json]`を追加。地域別局一覧から対象局を重複排除し、単一日を最大4並列で毎回取得する。部分失敗を表示し、JSONのcompleteと終了コードで欠落を通知する。2026年10月15日に47地域の応答から109局・4,323枠の取得、fmt・clippy・全58テスト成功を確認。期間指定・分類／キーワード検索・TUIは未実装。

- 初期実装段階。放送日・放送状態、チャプターJSON解析に加え、会員ログイン・セッション確認・原子的保存／削除の検証用CLIを追加済み。会員ログインはユーザー確認済み。ライブ／タイムフリーのHLS・先頭サンプル取得CLIも追加し、実アカウントで地元OBS・エリア外TBSライブ、TBS前日／10日前の取得を確認済み。音声出力と連続再生CLIは後日の項目を参照。検索・TUIは未実装。
- 依存取得・Cargo.lock生成済み。2026年10月15日にMacOSX26.5 SDKを明示してビルド・既存テスト6件・CLI確認9件が成功。既定のMacOSX27.0 SDKではリンクに失敗する。fmt・clippyも導入後に実行し、整形適用後のfmtチェック・警告をエラー扱いしたclippy・テスト6件が成功。詳細は[実装状況](docs/implementation.md)を参照。
- 2026年10月14日の東京エリアの番組表XMLで、番組ジャンル・出演者分類・タグの存在を確認済み。
- ADTS向けに純Rustの`rusty_aac 1.1.0`を追加し、地元OBS・エリア外TBSライブ、TBS前日／10日前の先頭各約5秒で、コア24kHzから48kHz・ステレオへのSBR再構成と全パケットのPCM生成を確認済み。fmt・clippy・テスト43件が成功。PSの明示シグナル・聴感品質・音声出力・連続再生は未検証。非ADTSのHE-AACは未対応で、取得成功やサンプルのPCM生成を30分連続再生対応済みとしない。
- 2026年10月15日に公開実装で公式AIチャプターの取得処理を確認。取得先・フィールド・参考リンクは設計書の「タイムフリーのチャプター」に記載。当日時点では本プロジェクトでの実通信、`eventId`の対応付け、認証条件、ジャンプ再生は未検証。取得の更新は2026年10月16日の項目を参照。
- 検証結果を記録する際は、確認日・対象・手順・結果を示す。取得できた番組情報だけを根拠に再生対応済みとは判断しない。

## 開発の順序

1. 公式Webにログインできるダブルプラン会員で、認証、ライブ、通常タイムフリー、8〜30日前の配信取得を検証する。
2. 実配信のコンテナとコーデックを確認し、純Rustによるデコードと連続再生を検証する。
3. 全国の番組表取得、検索、検索条件保存を実装する。公式AIチャプターの取得と放送枠への対応付けも検証する。
4. チャプター一覧とジャンプ再生を含むTUIを統合し、操作性と終了処理を検証する。

再生方式やAPIを推測で固定しない。技術的に成立しない条件が見つかったら、再現手順と根拠を記録して報告する。外部プレイヤーやOSのデコードAPIへ自動的に方針変更しない。

## 構成と実装ルール

- 単一のCargoプロジェクトで、`radiko`、`catalog`、`player`、`ui`、`app`、`domain`、`storage`の責務を分離する。アプリ状態は単一イベントループで更新し、処理領域ごとの要求ID・世代で古い結果を破棄する。
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
- 取得先候補は`https://api.annex-cf.radiko.jp/v1/programs/{eventId}/chapters`。公開実装を根拠とする候補であり、実通信検証前に確定仕様として扱わない。番組表XMLの番組IDと`eventId`を同一視しない。
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
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
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

録音、音声の永続保存、ポッドキャスト、通知、再生位置保存、他OS対応は追加しない。ユーザーが範囲を変更した場合は、その指示を反映して設計書を更新する。
