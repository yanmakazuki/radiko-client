# Repository Guidelines

## Scope and Architecture

This macOS Rust client provides filtered station and regional/nationwide date-range listings, authentication, HLS probes, bounded audio output, saved search CLI, and a search/live/time-free TUI. Chapter jumps require a matching event origin and slot; broader real-device validation remains unfinished. Read [design.md](docs/design.md) and [architecture.md](docs/architecture.md) before implementation; update both when requirements change. Record dated verification evidence and limitations in [implementation.md](docs/implementation.md). Preserve pure Rust decoding; do not substitute external players or OS decoders automatically.

## Project Structure

- `src/app.rs` and `src/app/search.rs`: main-loop-owned search state, editing, request generations, and bounded cancellable acquisition; `src/tui.rs`: Ratatui search/playback frontend and terminal restoration.
- `src/app/playback.rs`: bounded TUI playback commands, session resolution, snapshots, and output shutdown.
- `src/app/chapters.rs`: chapter request generations, bounded acquisition, and origin/slot-bound jump validation; listening permission is rechecked by playback.
- `src/app/auth.rs`: asynchronous session checks, explicit login/logout jobs, and contract-only snapshots; credential drafts are zeroized and never logged.
- `src/tui/filters.rs`: local date/scope/classification/keyword editing, validation, and observed-classification display; no I/O.
- `src/tui/viewport.rs`: global list/page selection and visible row ranges; format only visible rows. `src/tui/failures.rs`: read-only current acquisition failures and malformed-slot provenance. `src/tui/scroll.rs`: bounded wrap-aware help/chapter-summary scrolling. `src/tui/help.rs`: read-only key guide.
- `src/app/named.rs` and `src/tui/named.rs`: serialized local saved-search operations and browser; recalls populate drafts without fetching.

- `src/domain/search.rs`: validated criteria and startup date reset; `src/storage/searches.rs`: atomic versioned criteria storage; `src/searches.rs`: offline named-search management CLI.

- `src/catalog/search.rs`: pure AND/OR filtering and observed classification aggregation.

- `src/chapters.rs`: read-only chapter CLI; `src/radiko/stream/chapters.rs`: metadata identity checks and bounded chapter acquisition.

- `src/radiko/http/diagnostics.rs`: opt-in bounded request-stage timings for active HLS workers; no URLs, headers, bodies, or audio callback work.
- `src/player/state.rs`: owned playback snapshots and active playback states; `src/player/diagnostics.rs`: bounded worker-stage wall-clock measurements, independent of the audio callback.
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

These build, inspect CLI behavior, check formatting, reject lint warnings, and run tests. For the documented macOS SDK linker failure, prefix Cargo commands with `SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk`. Maintain `Cargo.lock`. The macOS Checks workflow runs stable formatting/lints and Rust/Python tests, plus Rust 1.85.0 tests; use `--locked` in CI and keep live probes out of automatic jobs.

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

- 2026年10月23日に公式のRust／Cargo 1.85.0を一時配置し、固定Cargo.lock・オフライン・全ターゲット／全機能の通常Rust 283テストが成功。通常環境はRust 1.98.1でfmt・clippy・283テスト成功。macOSのstable／1.85.0を検査するChecksワークフローを追加し、YAML構文を確認。GitHub上の実行は未実施。

- 2026年10月23日にASC付きAACの有界デコードとコンテナ先頭プローブを追加。通常Rust 282・fmt・clippy成功。実音声118パケットはADTS／通常ASC／明示SBRで48kHz・2ch・241,664フレームが一致し、メモリ内で詰めたMP4の先頭も2,048フレーム・SBR検出あり。保存・音声出力なし。実際の非ADTS配信と連続再生・PSは未確認。後続の4MiB上限PCM生成APIは生成AACで確認し、通常Rust 283・fmt・clippy成功。コンテナ出力へは未接続。

- 2026年10月23日に追加12局×2日の公開745枠を新規照合し、すべて原点一致。異なる原点の補正は実例がなく追加しない。認証・章本文・音声の取得や番組保存なし。PCM受領修正のletチェーンをRust 1.85で利用できる構文へ置換し、Rust 1.98.1でfmt・clippy・全Rustテスト成功。1.85自体でのビルドは未検証。

- 2026年10月23日の固定cec75bd1でFMJライブ1800.3秒・57操作・検索確定7回・章要約切替5回・最終不足0、明示停止・終了0・端末復元を確認。最長取得1,950ms・待機5,035→0ms・各失敗と再試行0。検証中のみアイドルスリープを抑止し終了で解除。

- 2026年10月23日の固定cec75bd1でユーザー参加のYamaha USB切断・再接続後の明示再生32.8秒・再再生5.2秒・不足0・終了と端末復元を確認。既定出力は別途読取でYamaha→内蔵→Yamaha、機器エラーは観測せず。後続で検証の終了JSONへ両回答時の数値集計を追加し、Python 23テスト成功。既存ストリームの出力先追跡とはしない。

- 2026年10月23日に受領計数修正後cec75bd1で30日前のTBS 9月23日枠を1800.8秒・57操作・検索確定7回・不足0、再再生5.2秒・不足0、終了0・端末復元まで確認。caffeinate -iで検証中だけアイドルスリープを抑止した条件。取得最大2,594ms・待機10,069→10,069ms・失敗と再試行0、不足観測0件。通常Rust 274・Python 22・fmt・clippy成功。

- 2026年10月23日に受領済みPCMの計数競合を生成AACで再現（期待3に対し4）。PcmReceiverで非待機受信と減算を同じ短いロックで同期し、受領済み分の二重減算も防いだ。取消・切断と取得開始時の残量を含む通常Rust 274・fmt・clippy、修正後の明示生成AAC47.23秒が成功。旧0394ede6の30日前枠は新ビルド切替のため直近1721.5秒・不足0で中断し、スクリプト130・子0・端末復元を確認、30分成功とはしない。

- 2026年10月23日に固定0394ede6のFMJライブを1800.5秒・57操作・検索確定7回・最終不足0、明示停止・終了0・端末復元まで確認。最長取得2,718msの待機PCMは5,035→0msで、不足0だった。停止時pipelineもJSONへ保持。

- 2026年10月23日に検証スクリプトへ不足増加時の数値観測を追加。通知最大8件・直近1件・観測増加数だけを保持し、通常停止のsummaryを再再生後にも維持する。Python 21テストで上限・数値保持・停止分類・機密文言除外を確認。実配信の不足増加時の経路は未確認で、増加を読んだ時点の診断を原因や音切れ回数とは扱わない。

- 2026年10月23日に共通検証へ--timefree-endを追加。固定0394ede6のTBS末尾10.0秒・不足0とRN1自然終了187.4秒・不足0で、3秒維持・次枠への移行なし・終了0・端末復元を確認。Python 20テスト成功。30日前のTBS 9月23日枠も62.6秒・不足0と再再生5.3秒・不足0を確認したが、期限直前／直後の保証ではない。RN1試行開始後のライブ末尾10秒の操作抑止は実機未再確認。

- 2026年10月23日に組付き診断の固定a070e07dでTBS 10月6日枠を1800.9秒・57操作・検索確定7回・最終不足0、再再生5.2秒・不足0、終了0・端末復元まで確認。生成AACの11秒待ちも開始／最少の組を実機確認し、スレッド間受領の通常試験を追加。通常Rust 273・Python 18・fmt・clippy成功。

- 2026年10月23日に検証JSONのstopped／completeへ本測定のpipelineを追加。固定0394ede6の30.6秒・不足0と再再生5.2秒・不足0で、再再生後も本測定の診断を保持し終了・端末復元を確認。処理／通信の直近観測であり、全通信の停止後集計とはしない。

- 2026年10月22日の固定d8d50771はTBS 10月6日枠で435.1秒・14操作・不足0まで観測後、ユーザーの区切り指示で中断。スクリプト130・子0・端末復元を確認し、30分成功とはしない。後続の最長取得と開始／最少待機PCMの組付き診断は仮想時刻・表示・数値読取で検証し、この実機試行とは分ける。最終の通常Rust 272・Python 18・fmt・clippy成功。

- 2026年10月22日に待機PCMの個数と時間を診断へ追加。キュー2＋配送待ち1で最大3、受領・取消・切断で減算し、現在PCMと機器リングを含めない。生成AAC・音量0で同じ11秒待ちを入れ、早い側は待機0・不足約7.019秒、先読み後は待機2→0・不足0を確認。通常Rust 271（HTTP2・実機1はignored）・Python 18・fmt・clippy成功。

- 2026年10月22日に固定19ff0b40でミュート再生中の全国30日検索を84.79秒・132,614枠の部分取得（地域／局日失敗0・不正4）で確認。出力90.2秒・最終不足0、ヘルプ8回最大272.2ms、終了0・端末復元。

- 2026年10月22日にoutput-info [SAMPLE_RATE]を追加。既定機器名とf32・mono/stereoの対応宣言を再生と同じ条件で読む。音声・通信・保存・OS設定変更なし。固定19ff0b40で内蔵スピーカーの48kHz・2ch宣言と24kHz非対応宣言を確認。同ビルドRN1は30.8秒・5操作・不足0、再再生5.2秒・終了と端末復元。通常Rust 270（ローカルHTTP2件はignored）・fmt・clippy成功。

- 2026年10月22日に公開6局×4日656枠のイベント情報を上限付きで新規照合し、すべて原点一致。異なる原点の実例は得られず、補正は追加しない。章の一括取得・聴感精度・権限の検証ではない。

- 2026年10月22日にローカル偽HTTPで503／本文途中切断→200の再試行、試行2・待機1、元の応答番号保持と部分本文破棄を確認。通常Rust 269（明示HTTP2件はignored）・明示HTTP2件・fmt・clippy成功。実配信の遅延原因や実時間バックオフの証明とはしない。

- 2026年10月22日に固定767fae38でRN1 12:53〜13:00枠のミュート242.1秒・7操作・不足0、自然終了と次枠への移行なし・終了0・端末復元を確認。検索確定0回で30分検証とはしない。

- 2026年10月22日に固定4764dd21で29日前のTBS 9月23日12時枠を1800.4秒・57操作・検索確定7回・章要約切替5回・最終不足0、再再生5.2秒・不足0、終了0・端末復元を確認。後続の操作一覧表示修正は別ビルド。

- 2026年10月22日にHTTP未取得で終わる停止後の通信診断が操作一覧から消える回帰を修正。未計測の行だけを除外し、80×20で応答前失敗の回数と応答未取得を表示テストで確認。通常Rust 269・Python 18テスト、fmt・clippy成功。検証スクリプトもSIGHUPで後処理へ接続し、隔離プロセスで一時領域削除を確認。4764dd21による29日前枠の30分検証とはビルドを分ける。

- 2026年10月22日にctrlcのtermination機能を有効化。SIGTERMがハンドラーを迂回する修正前の失敗を再現し、変更後はSIGTERM／SIGHUP／SIGINTの隔離テストと固定4764dd21のセッションなしTUI終了0・端末復元を確認。同ビルドのミュート再生5.0〜5.1秒後も3種類各終了0・端末復元。通常Rust 268・Python 17テスト、fmt・clippy成功。認証更新中やSIGKILLの復元を保証しない。

- 2026年10月22日に診断へ現在／直近試行のHTTP応答番号を追加（新試行では未取得、本文失敗でも200を残す）。固定87e8a692でユーザー操作のスリープ復帰後のoutput_stalledとHTTP 200／通信失敗0、操作一覧の応答、明示再生32.6秒・再再生5.2秒・各不足0・終了と端末復元を確認。通常Rust 267・明示ローカルHTTP1・Python 17テスト、fmt・clippy成功。

- 2026年10月22日にタイムフリーの一覧要求長を冒頭・シークとも60へ統一（ライブ15・PCM容量2・出力リング2秒を維持）。固定e68dd928でTBS 10月6日枠を1800.3秒・57操作・最終不足0、再再生5.2秒・終了と端末復元まで確認。通常枠1800.7秒・FMJライブ1800.6秒・枠末尾10.0秒も不足0。遅延は再現せず、不足低減の効果は断定しない。通常Rust 266・明示ローカルHTTP1・Python 17テスト、fmt・clippy成功。--liveは指定秒数の明示停止、--live-endは自然終了と区別する。

- 2026年10月22日の固定0f13dd20でTBS 10月6日枠を1800.5秒・57操作・最終不足0、再再生5.3秒・不足0、終了0・端末復元まで確認。後続の診断追加とはビルドを分ける。

- 2026年10月22日にエラー出口の直近診断通知と通信失敗／期限超過／接続失敗の有限カウンター、操作一覧の通信行折り返しを追加。固定01ea3ee1の60.7秒・14操作・不足0、再再生5.3秒・終了と端末復元を確認。通常Rust 266／Python 14テスト・fmt・clippy、明示実行のローカルHTTPタイムアウト1件が成功。エラー時の値は停止後の最終集計ではない。

- 2026年10月21日の0f13dd20でRN1ライブを1248.3秒・40操作・不足0まで観測し、ユーザーの区切り指示で中断。子終了0・端末復元を確認し、自然終了・30分成功とはしない。公開チャプター6局12枠は原点一致で、異なる原点の補正は未対応。通信診断追加後はRust 262／Python 14テスト・fmt・clippyが成功。詳細はdocs/implementation.mdを参照。

- 2026年10月21日に再生中HLSの応答待ち・本文受信・再試行待ちを有限の通信診断へ分離。応答待ちはDNS／接続／TLS等を含み、個別原因を断定しない。36行以上のTUIと操作一覧へ表示し、終了後は直近未集計として保持。30行の章表示回帰を修正し、Rust 262／Python 14テスト・fmt・clippy成功。先読み2の15e18c16による30分結果とはビルドを分ける。

- 2026年10月21日に先読み2の固定15e18c16で1800.9秒・38操作・検索確定4回・章要約切替3回、最終不足166,400フレーム（約3.467秒）・終了0・端末復元を確認。再再生5.2秒は不足0。取得最大観測10,759ms・復号最大793ms。先読みを増やしても不足は残り、条件を揃えた比較ではないため効果は断定しない。

- 2026年10月21日にPCM先読みキューを2セグメント（各最大4MiB）へ増やし、満杯時の追加取得停止・取消を生成音声で確認。機器リングは2秒のまま。固定15e18c16で1000秒位置から60.6秒・14操作・不足0、再再生5.2秒・不足0、終了0・端末復元。取得最大1,603msの短時間確認であり、約11秒の取得待ちの解消や30分の確認とはしない。Rust 260／Python 13テスト・fmt・clippy成功。

- 2026年10月21日に固定47047852でユーザー操作のMacスリープ復帰後のoutput_stalledを確認。同じTUIで操作一覧が応答し、明示的な再生し直し31.1秒・7操作・不足0、再再生5.2秒・不足0、終了0・端末復元を確認した。--sleep-checkで再現可能とし、通常のPython 11テストは偽入力／画面のみ。

- 2026年10月21日に診断追加後の固定ビルド2acaa0a7でTBS 10月6日枠を1800.2秒ミュート出力。独立40×160 PTYで38操作・検索確定4回・章要約切替3回・最終不足0・終了0・端末復元を確認。

- 2026年10月21日に再生ワーカーの取得・復号・配送待ちの数値診断を追加。検証子プロセスの親制御端末継承を修正し、独立した40×160で60.2秒・不足0と診断値、80×20で取得失敗表示・終了と端末復元を確認。通信・復号なしのprobe-output --muteを追加し、無音180秒・不足0と、別試行の3秒後SIGINT・中断表示・終了0を確認した。現在の既定出力は読取時点で内蔵スピーカーだった。実施範囲とビルドはdocs/implementation.mdを参照。

- 2026年10月20日にFMJライブをUSB機器から25秒・アプリ音量25%で出力。48kHz・2ch・5セグメント・不足0・指定時間停止・終了0、ユーザーの「自然に聞こえ、音切れ・雑音は気にならなかった」を確認。全局・全形式の品質保証とは区別する。

- 2026年10月20日にヘルプと章要約の折り返し後スクロール、Tabによる要約拡大を追加。停止と配信終端受信の競合を生成データで再現・修正し、停止要求がなければ本来のエラーを残す。Esc二連送信で条件フォームが残る検証手順も修正。ビルドa7007461の8.4秒停止・再開後60.5秒・14操作・不足0・端末復元、Rust 248／Python 6テスト成功。30分停止の別ビルドの結果と区別する。

- 2026年10月20日に番組・ライブ・章一覧のページ／先頭末尾移動と、eで今回の取得失敗の地域・局／日を閲覧する専用画面を追加。新規検索で古い失敗を消去し、閲覧キーを通信・認証・再生へ転送しない。全国30日132,595枠の部分取得を152.61秒・ピークRSS約476MB・ヘルプ応答最大214msで確認。全件取得成功とはしない。

- 2026年10月20日にユーザー操作のUSB機器切断・再接続と、再接続後の明示的ミュート再生15.6秒・不足0・端末復元を確認。切断時も出力が進み機器エラーは発生せず、OSの出力先切替の可能性がある。実機エラー経路は未検証。再試行では1,800.4秒の停止・位置保持・再開後30.5秒出力・不足0と端末復元に成功。422.2秒までを観測して判定中断した前試行とは区別する。

- 2026年10月19日にユーザーが新TUIで実アカウントのログイン成功を確認。保存権限0700／0600・versionと保存項目を内容非表示で確認し、新セッションでライブから章への切替・同枠シーク・124.8秒の停止位置保持と再開・端末復元が成功。一時ストアは削除した。現在の確認範囲と残る項目はdocs/validation.mdを参照。

- 2026年10月19日に番組一覧の可視行だけを整形する描画へ変更。合成50,000枠のデバッグ描画で約210〜217msから約3.5〜5.7msとなり、全体選択と端末サイズ変更・結果縮小を検証。fmt・clippy・全240テスト、固定ビルドでの60.8秒のミュート操作回帰が成功した。実機の全国30日検索全体の負荷測定とは区別する。

- 2026年10月19日にAAC取消・操作一覧を含む固定ビルドでTBSの20261006120000（13日前）を1800.5秒ミュート出力。39操作サイクル・検索確定4回・最終不足0・正常終了と端末復元が成功した。後続の大量一覧描画変更とはビルドを分けて記録する。

- 2026年10月19日にRN1の20:20〜20:50枠でミュート出力769.5秒・16操作サイクル・最終不足0を確認。自然終了時の番組終了表示、次枠へ移らないこと、正常終了と端末復元が成功した。別の前回試行の終了待ちタイムアウトと区別して実装状況へ記録。

- 2026年10月19日にAACの世代取消をADTS走査・パケット前後へ接続。部分PCMとデコーダー状態を破棄し、単一パケットの途中中断はしない。?による読み取り専用操作一覧も追加し、入力欄の文字入力と区別する。全238テストと実配信の短時間開閉確認が成功。

- 2026年10月19日に最新ビルドの固定コピーでTBSの20261015120000を1800.3秒ミュート出力し、38操作サイクル・検索確定4回・最終不足0・正常終了と端末復元を確認。早期終了を検索0件時の検証スクリプトのEsc送信で再現し、画面の開閉確認を修正した。scripts/verify_tui_playback.pyで明示的に再検証できる。詳細とビルドのハッシュは実装状況を参照。

- 2026年10月19日に放送状態とは別の配信確認表示を追加。局・開始終了・モードと出力済みフレームが一致する現在の再生だけを確認済みとし、停止後や準備だけの状態は未確認へ戻す。新しい再生の権限判定は省略しない。現在章も出力前には強調しない。

- 2026年10月18日にユーザーの聴感で、TBSの10月15日・10月6日12時枠の交通情報とチャプター開始秒数の対応を確認。原点一致のEventBindingを持つ章だけEnterから通常の権限確認・シーク／別枠切替へ接続した。原点が異なる枠の補正は未対応。区切り時点の検証・中断状況は実装状況を参照。

- 2026年10月18日に共通HTTPと会員認証を非同期reqwestへ移行し、blocking featureを削除してCargo.lockを更新。共有current-thread Runtimeで要求futureを取消し、DNSは最大4件の許可をOS作業終了まで保持する。検索・チャプター・会員確認・再生準備とHLSへ各領域の取消を接続。認証POSTの自動再試行は追加しない。実配信の短時間シーク・再開・末尾停止・ログアウト検証が成功し、30分検証とはビルド／範囲を分けて記録する。

- 2026年10月18日にTBSの20261015120000をミュートで1800.6秒連続出力し、38回の検索編集取消・フォーム開閉・再取得・ライブ画面往復・チャプター閲覧の操作サイクルを確認。定期観測の不足はすべて0、停止・q終了・端末復元が成功した。ライブ一覧追加時点のビルドでの一ケースであり、後続の認証／名称選択、聴感、長時間ライブや機器切断までの検証とはしない。詳細な範囲は実装状況を参照。

- 2026年10月18日に地域欄のF2で47都道府県名の複数選択を追加。全国／複数地域を下書きへ反映し、検索確定まで通信・再生操作を行わない。単一局を地域指定へ置換することを明示し、取消では元の入力を維持する。地域名は静的表示であり局一覧は検索時に新規取得する。

- 2026年10月18日に条件フォームのF2で分類名称の複数選択を追加。候補は編集開始時点の観測データだけを用い、同ID別名・保存済み未観測ID・未分類を保持する。Enterは下書きへの反映のみで、検索確定までは通信しない。カンマを含む保存IDを保持し、直接入力も継続利用できる。

- 2026年10月18日にTUIのIログイン／Oログアウトを追加。入力中と取消ではI/Oせず、パスワード非表示・Zeroizing保持、認証成功後のみ原子的保存。ログアウトはy確認後に再生を停止して指定ストアのセッションだけを削除する。更新は単一ジョブで重複を受理せず、古い認証確認を破棄する。終了時の結果未確認は端末復元後に案内。実アカウントの新ログイン送信は未検証。

- 2026年10月18日にvで専用ライブ一覧を追加。当日を新規取得し放送中だけ表示、Enterでライブ再生。vで元のタイムフリー条件・期間へ戻って再取得する。開始／終了境界では通信なしで表示を更新し、画面切替だけで音声を変更しない。明示した過去日付はタイムフリーへ移して保持する。長時間再生と同時操作の検証は1800.6秒の実施記録を参照。

- 2026年10月18日に再生CLIの初回配信解決と対話中のシーク／切替準備も取消対応へ接続。Ctrl+Cは会員確認／解決が戻った後に中断扱いとし、要求別取消と5秒定期通知を両立する。blocking I/Oの即時中断と実配信での再検証は未実施。

- 2026年10月18日にTUI再生準備のfactoryへ要求別StopTokenを接続。セッション読込・会員確認の前後と、配信解決HTTPの区切りで取消を確認する。旧準備の取消／最新要求開始を偽factoryで検証。会員確認の実行中通信とblocking I/Oそのものの中断は未対応。

- 2026年10月18日に再生中HLSのHTTPへ世代キャンセルを接続。停止・切替後の追加要求／再試行／リダイレクトと本文読取の区切りで取消し、部分本文を破棄する。blocking通信そのものと再生準備／会員確認の途中中断は未対応。通常テストは偽source／readerで検証し実通信しない。

- 2026年10月17日にTUIの出力診断を追加。現在の出力世代の秒数・不足フレーム／秒数・PCM形式・セグメント数を表示し、正常停止時の最終値を保持する。エラー時の通知値は終了時未集計と明示する。長時間検証手順をREADMEに記載。30分実配信の同時操作は未実施。

- 2026年10月17日にTUIの`g`で整数秒指定シークを追加。入力・取消では再生を維持し、確定要求には局ID＋開始日時を保持してワーカーでも照合する。ライブ・範囲外を拒否する。実配信TUIの位置確認と30分同時操作は未検証。

- 2026年10月17日にTUI一覧の`l`で選択枠のライブ再生を追加。放送前・終了済みを通信前に拒否し、準備・再開時も局＋開始時刻を保持する。専用ライブ一覧画面とTUI実配信の長時間検証は未完了。

- 2026年10月17日にbによる名前付き検索条件の保存・削除・呼び出し画面を追加。保存日付・期間を含めて編集画面へ渡し、検索確定まで取得しない。ファイル操作は単一ワーカー、表示世代で古い応答を破棄し、受理済み更新は画面を閉じても中断しない。仮想端末で保存・同名更新・削除取消／確定・期間呼び出し・保存直後の終了と端末復元を確認。fmt・clippy・全185テスト成功。実配信30分の同時操作、ライブ画面、チャプタージャンプは未検証／未接続。

- 2026年10月17日にfで全検索条件を編集するTUIフォームを追加。日付・期間・地域／局・分類ID・キーワード・軸別未分類を、確定まで通信せず編集する。入力検証・取消、観測分類の参考表示、UTF-8カーソル操作に対応。仮想端末で不正日付拒否・取消・JP44の2日間検索・条件保存と端末復元を確認。fmt・clippy・全175テスト成功。名前付き条件の画面呼び出し、実配信30分と同時操作は未実装／未検証。

- 2026年10月17日にTUI起動時の保存セッション確認とa再確認を追加。検索・再生から独立して、未ログイン・破損／未対応形式／不正権限・失効・通信失敗と契約フラグを表示する。認証情報は状態・通知へ渡さず、ファイルを変更しない。古い確認結果を破棄し、再生時の再確認は維持する。仮想端末で破損表示・ファイル保持・a再確認・終了時の復元を確認。fmt・clippy・全168テスト成功。実アカウントでの新しい表示経路の再検証、ログイン／ログアウトのTUI入力は未実施／未接続。

- 2026年10月17日にTUIのcによる公式AIチャプター閲覧を追加。r再取得、上下／j k選択と要約、Escで取消・破棄。独立した要求IDと単一取得・最新待機要求で古い応答を破棄し、音声位置未検証の候補はジャンプ・現在位置強調へ接続しない。TBSの10月15日12時枠で13件表示・再取得・開き直し・ジャンプ拒否と端末復元を確認。fmt・clippy・全160テスト成功。チャプタージャンプ、30分同時操作は未検証／未接続。

- 2026年10月17日にTUIのタイムフリー再生を接続。一覧Enter、Space一時停止／再開、s停止、左右15秒シーク、+／-音量に対応。容量4の操作と最新状態通知を独立サービスへ渡し、音声出力制御は専用スレッドで行う。保存済みセッションは再生準備時に検証し、放送前は通信しない。生成AACのミュート実機出力で一時停止位置・再開・停止、仮想端末で未ログイン時の表示と端末復元を確認。fmt・clippy・全150テスト成功。実配信とTUIの30分同時操作、ライブ画面・チャプターは未検証／未接続。

- 2026年10月16日に閲覧専用`tui`を追加。番組一覧／詳細、キーワード編集・確定・取消、再取得、条件の当日復元・終了時保存に対応。appの検索世代・最新待機要求・容量4の通知とcatalogの協調キャンセルで古い結果を破棄し、全体4並列を維持する。仮想端末で公開番組表表示・編集・q／Ctrl+C／SIGINT終了と端末復元を確認。fmt・clippy・全143テスト成功。画面からの再生・チャプターと30分同時操作は未接続／未検証。

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
