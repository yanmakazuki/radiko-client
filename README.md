# radiko-client

macOS向けRust製radiko TUIクライアントの初期実装です。現在は局別・地域別・全国の日別番組一覧、会員認証、ライブ／タイムフリーのHLS・先頭サンプル取得、AAC/SBRのPCM生成、サンプル／連続音声出力、公式チャプター取得・ローカルJSON解析の検証用CLIを実装しています。ログインはユーザー確認済みで、配信取得も実アカウントで確認しました。連続再生CLIは短時間検証済みで、単日・期間の分類／キーワード検索CLIに対応し、条件保存にも対応しています。閲覧専用TUIも追加しました。TUIからの再生と30分連続再生の検証は未完了です。

局・放送日を指定して番組一覧を取得できます。ログイン不要で、実行ごとに再取得します。

```sh
cargo run -- programs TBS
cargo run -- programs OBS 20261015
cargo run -- programs TBS 20261015 --json
cargo run -- programs --areas JP13,JP14 20261015
cargo run -- programs --all 20261015 --json
```

日付省略時は日本時間の午前5時を境界とする当日の放送日です。番組名・出演者・番組ジャンル・出演者分類と、取得時点の放送状態を表示します。翌日未明と放送前の番組も含みます。再生可否は未確認として扱います。JSONには説明・タグも含みます。`--areas`はその地域で聴ける局の一覧をもとに選択します（JP1〜JP47、カンマ区切り）。`--all`は47地域を取得します。地域間で共有する局を重複排除し、指定した1日分を最大4並列で取得します。テキスト表示は取得できた局から順次表示し、JSONは最後にまとめて出力します。進捗は標準エラーに出力します。

地域・局の取得失敗や不正枠除外がある場合は、取得できた結果と欠落範囲を表示し、終了コード1を返します。JSONの`complete`も`false`になります。失敗した地域の対象局数は未確定です。同じコマンドを再実行すると全件を再取得します。単日・期間の分類／キーワード検索と条件保存に対応しています。閲覧TUIでは放送状態を時刻から更新し、番組表の再取得は手動で行います。

```sh
cargo run -- login
cargo run -- session
cargo run -- logout
```

`login`は対話端末でメールアドレスとパスワードを入力します。パスワードは非表示で、保存しません。ログイン後にセッションの有効性を別途確認し、エリアフリー・タイムフリー30の契約情報を表示します。契約情報だけで個別番組が再生可能とは判断しません。

保存するのは再利用可能なセッションのみです。保存先は`directories`のユーザーデータディレクトリ内の`session.json`です。`--data-dir DIR`で変更できます（例：`cargo run -- --data-dir ./local-data login`）。専用ディレクトリを指定してください。Unixではディレクトリ0700・ファイル0600が必要で、新規作成時に設定します。セッションは平文のため、Gitなどで共有しないでください。

`session`は保存済みセッションを自動で読み込み、サーバーで確認します。ファイルがない場合は未ログイン、破損・未対応形式なら理由を表示して元ファイルを保持します。`logout`は通信せずローカルのセッションを削除します（公式Webなど別端末のログインは変更しません）。明示的な`login`が成功したときだけ保存を置き換えます。

連続再生は次のコマンドで行います。既定では選択した番組枠の末尾で停止し、次の番組へは移りません。日時は日本時間です。

```sh
cargo run -- play-live OBS
cargo run -- play-live TBS --seconds 30
cargo run -- play-timefree TBS 20261015120000 --seconds 30
cargo run -- play-timefree TBS 20261006120000 --seconds 30
```

`--seconds N`（1〜86400）は出力するメディア時間の上限です。省略時は番組末尾まで再生し、ライブは終了日時も上限にします。`Ctrl+C`で停止できます。保存済みセッションと個別番組の配信条件を確認してから取得します。タイムフリーは指定時刻を含む放送枠の冒頭から再生し、`--offset SECONDS`で開始位置を変えられます。

HLSを更新しながら通し番号順に取得し、同じデコーダーと出力ストリームを維持します。音声を保存せず、PCMの待機キューは1セグメント、出力リングは2秒分に制限します。欠落・形式変更・機器エラー・更新停止は理由を表示して停止します。HTTPやデコードは音声コールバックから実行しません。Ctrl+Cで音声を止め、待機中の処理を取り消しますが、すでに実行中のblocking HTTPは直ちに中断できません。準備中の通信にも既存のタイムアウト／再試行上限が適用されます。

2026年10月16日にライブとタイムフリーの30秒連続出力を確認しました。30分連続再生、聴感品質、実際の番組終了境界、機器切断、検索・TUIとの同時操作は未検証です。対応形式はADTS/AAC、mono/stereo・f32・PCMと同じレートの出力機器に限ります。

タイムフリーの開始位置指定と再生中のシークにも対応しています。

```sh
cargo run -- play-timefree TBS 20261015120000 --offset 60 --seconds 10
cargo run -- play-timefree TBS 20261006120000 --interactive
```

`--interactive`では次を入力し、Enterで確定します。

- `seek 120`：番組冒頭から120秒の位置へ移動（小数も指定可能）
- `+15`／`-15`：出力済みPCMの位置から15秒進む／戻る
- `q`：停止して終了

シーク先の認証・配信条件と時刻を確認し、2秒分（末尾付近は残り分）のPCMと音声出力機器を準備してから切り替えます。準備中・失敗時は元の音声を継続し、連続した操作では最新の要求だけを反映します。ライブ・放送枠外のシークは拒否します。`--seconds`の上限はシーク成功後に改めて適用します。シーク先の準備前に現在の再生が終了した場合は、そのまま終了します。

対応はHLSの`PROGRAM-DATE-TIME`で位置を確認できる配信に限ります。確認できなければ推測せず切替を拒否します。表示位置はHLS時刻と出力コールバックへ渡したフレーム数に基づき、聴感上の同期精度は未検証です。矢印キーや局切替はTUI統合時に追加します。

音声出力の検証も明示的なコマンドで実行します。既定の出力機器から音が出ます。

```sh
cargo run -- probe-output 1                     # 1秒の小さい440Hz生成音（1〜10秒、省略時2秒）
cargo run -- play-sample OBS                    # 地元ライブの先頭1セグメント
cargo run -- play-sample TBS                    # エリア外ライブ（契約が必要）
cargo run -- play-sample TBS 20261014120000     # 指定枠のタイムフリー冒頭1セグメント
```

`play-sample`は保存済みセッションを確認し、既存の放送枠・契約・地域・配信条件の判定を通してから取得します。`--data-dir DIR`も使えます。ADTS/AACのみが出力対象で、PCMを最大4MiBまで検証してから、2秒分のバッファを介してCPALへ送ります。出力ゲインは0.25固定、mono/stereo・f32・PCMと同じレートに対応する機器を使用し、形式が合わなければ理由を表示します。OSの音声デコーダーや外部プレイヤーは使いません。

先頭約5秒などの**1セグメントだけ**を出力して停止します。ライブでは選択枠の残り時間も上限にします。表示するフレーム数は音声コールバックへ渡した数で、耳に届いたことや音質を保証する指標ではありません。末尾は250ms待って出力ストリームを解放します。大きな機器遅延での末尾、実機の切断や放送終了境界は未検証です。連続再生は上記の専用コマンドを使用してください。タイムフリーのシークと局・番組切替は、連続再生コマンドの`--interactive`で利用できます。

配信取得の検証は明示的に実行します。保存済みセッションを使い、放送枠・契約・地域・番組の配信条件を確認してからHLSと先頭の1セグメントを取得します。

```sh
cargo run -- probe-live TBS
cargo run -- probe-timefree TBS 20261014120000
cargo run -- probe-timefree TBS 20261005120000
```

日時は日本時間の実日付による`YYYYMMDDHHmmss`で、時は00〜23を指定します。指定時刻を含む放送枠全体を解決し、その冒頭のサンプルを確認します。過去の日付は実行時の聴取期限に合わせて選び直してください。`--data-dir DIR`も指定できます。

結果には放送枠、認証エリア、HLS宣言コーデック、サンプル形式、デコード確認の成否を表示します。Cookie・トークン・配信URL・音声データは出力／保存しません。終了コード0はHLSとサンプルの取得成功を表し、デコード成功や連続再生の保証ではありません。

2026年10月15日にOBSの地元ライブ、TBSのエリア外ライブ・前日分・10日前のタイムフリーで、先頭約5秒の全パケットを純RustでPCMへ変換できました。ADTSは`rusty_aac 1.1.0`、その他のサンプル形式はSymphoniaを使用します。結果には入力のコアレート、PCMのレート・チャンネル数、SBR検出、パケット数、処理時間も表示します。確認した実音声はコア24kHzから48kHz・ステレオへのSBR再構成を通っています。

`probe-live`／`probe-timefree`は音声出力を行わず、PCMは1パケットごとに破棄します。2026年10月16日に`probe-output`と`play-sample`で短い音声を実機の出力経路へ渡せました。PSの明示シグナルと聴感品質、連続再生・番組末尾の停止は未検証です。CRC付き／複数raw blockのADTS、PCEによるチャンネル構成、非ADTSのHE-AACは今回の接続範囲外として拒否します。

```sh
cargo run -- --help
cargo run -- inspect-chapters tests/fixtures/chapters.json 90 0
```

引数はJSONファイル、選択した放送枠の秒数、イベント音声原点から放送枠開始への補正秒数です。合成fixtureでは補正を0としています。実番組の原点対応を確認する機能はまだなく、この操作で聴取可否やシークの正しさは検証できません。

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

この環境では既定のMacOSX27.0 SDKでリンクに失敗します。必要に応じて各Cargoコマンドに`SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk`を付けて実行してください。

[機能設計](docs/design.md) / [アーキテクチャ](docs/architecture.md) / [実装状況と検証の制約](docs/implementation.md)

Interactive playback also accepts `pause`, `resume`, and `volume 10` (Enter to confirm; default volume 25%, range 0..100). Volume zero mutes while playback advances. Timefree resumes from the paused position; live resumes at the latest position within the same program. `play-live` also accepts `--interactive`. Live playback stops at the selected program end even while paused.

対話再生中の局・番組切替（Enterで確定）：

```text
live OBS
timefree TBS 20261015120000
timefree TBS 20261006120000 --offset 120
```

切替先の準備中・失敗時も現在の再生を維持します。成功後に音量を引き継いで切り替え、以後のシーク・再開も新しい番組を対象にします。`--seconds`の上限は切替後に再適用します。

対話再生中の`status`で現在の番組名・局・放送日時（`+09:00`）・状態・音量・準備中の切替先を表示します。通信や再生切替は追加で行いません。同じ情報を5秒ごと、および操作・準備結果の反映時にも表示します。ライブは出力秒数を示し、未確認の番組内位置を再生位置として表示しません。番組名が取得できない場合は「番組名未取得」と表示します。

公式AIチャプターの取得・一覧確認（ログイン不要、日時は日本時間）：

```sh
cargo run -- chapters TBS 20261015120000
cargo run -- chapters TBS 20261006120000
```

毎回メタデータと対象チャプターを取得し、楽曲と不正位置を除外します。枠との対応が不明なら位置補正を推測しません。表示する秒数は参考値で、実音声との同期・ジャンプ再生は未検証／未接続です。TBSの前日枠で13件、10日前枠で14件の一覧取得を確認しました（2026年10月16日）。

番組一覧の絞り込み（毎回取得、単一局・地域・全国で利用可能）：

```sh
cargo run -- programs TBS 20261015 --keyword 生活
cargo run -- programs TBS 20261015 --classifications
cargo run -- programs TBS 20261015 --genre P003,P007 --personality C015 --keyword 生活 --json
cargo run -- programs --areas JP44 20261015 --genre-unclassified
```

分類IDは`--classifications`またはJSONの`observed_classifications`で確認できます。同じ分類内のIDはOR、ジャンル・出演者分類・キーワードはANDです。未分類は`--genre-unclassified`／`--personality-unclassified`で選択できます。キーワードは番組名・出演者・説明・詳細情報・タグの大小文字を区別しない部分一致です。

JSONの`source_program_count`は絞り込み前、`matched_program_count`は一致した件数です。観測分類も絞り込み前の今回の取得範囲から集計し、全分類とは扱いません。取得欠落や不正枠は検索結果が0件でも不完全として通知します。TUIではキーワードの編集・検索に対応し、地域・期間・分類の変更は起動引数で指定します。

期間を指定する場合は開始日の後に終了日を指定します（YYYYMMDD、両端を含む最大30放送日）。

```sh
cargo run -- programs TBS 20261014 20261015 --keyword 生活 --json
cargo run -- programs --areas JP44 20261014 20261015 --keyword 生活 --json
```

局一覧は期間全体で一度取得し、番組表は局×日を通じて最大4並列で取得します。JSONでは`date_range`、`successful_stations`（局数）、`successful_schedules`（局・日数）を区別します。部分失敗は局と放送日を表示し、取得できた分だけを返して終了コード1とします。日付省略は従来通り当日のみで、暗黙の30日一括取得は行いません。

検索条件の保存・呼び出し（保存・一覧・確認・削除は通信しません）：

```sh
cargo run -- searches save 生活 TBS 20261014 20261015 --keyword 生活
cargo run -- searches list
cargo run -- searches show 生活
cargo run -- programs --saved 生活 --json
cargo run -- searches delete 生活
```

通常の検索条件も前回分として保存します。`programs --restore`は日付を当日1日に戻して前回条件で再取得し、`searches restore`は復元結果を通信せず表示します。名前付き条件では保存した期間を適用します。混在する検索オプションは受け付けません。

保存先はセッションと同じデータディレクトリの`searches.json`です。`--data-dir DIR`で変更でき、セッション情報・番組表・音声は条件ファイルに含めません。破損ファイルは自動で上書きせず、保存失敗でも通常検索は続行します。TUIは引数なしで起動すると、前回条件を当日1日に戻して復元します。

閲覧専用TUI（ログイン不要）：

```sh
cargo run -- tui
cargo run -- tui --areas JP44
cargo run -- tui TBS 20261015
```

`j/k`または上下で一覧を移動し、右側で詳細を確認できます。`/`でキーワードを編集し、Enterで検索、Escで編集を取り消します。`r`で再取得、`q`またはCtrl+Cで終了します。編集中は通信せず、検索中も操作でき、古い検索結果は新しい一覧に反映しません。最後に確定した条件は正常終了時に保存します。

初回は当日・全国、引数省略時は前回条件の日付だけ当日に戻します。地域・期間・分類は`programs`と同じ起動引数で指定できます。**画面からの音声再生・チャプター操作は未対応**です。既存の`play-live`／`play-timefree` CLIを使用してください。
