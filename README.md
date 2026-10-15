# radiko-client

macOS向けRust製radiko TUIクライアントの初期実装です。現在は会員認証、ライブ／タイムフリーのHLS・先頭サンプル取得、ローカルのチャプターJSON解析の検証用CLIを実装しています。ログインはユーザー確認済みで、配信取得も実アカウントで確認しました。音声出力・連続再生・検索・TUIは未実装です。

```sh
cargo run -- login
cargo run -- session
cargo run -- logout
```

`login`は対話端末でメールアドレスとパスワードを入力します。パスワードは非表示で、保存しません。ログイン後にセッションの有効性を別途確認し、エリアフリー・タイムフリー30の契約情報を表示します。契約情報だけで個別番組が再生可能とは判断しません。

保存するのは再利用可能なセッションのみです。保存先は`directories`のユーザーデータディレクトリ内の`session.json`です。`--data-dir DIR`で変更できます（例：`cargo run -- --data-dir ./local-data login`）。専用ディレクトリを指定してください。Unixではディレクトリ0700・ファイル0600が必要で、新規作成時に設定します。セッションは平文のため、Gitなどで共有しないでください。

`session`は保存済みセッションを自動で読み込み、サーバーで確認します。ファイルがない場合は未ログイン、破損・未対応形式なら理由を表示して元ファイルを保持します。`logout`は通信せずローカルのセッションを削除します（公式Webなど別端末のログインは変更しません）。明示的な`login`が成功したときだけ保存を置き換えます。

配信取得の検証は明示的に実行します。保存済みセッションを使い、放送枠・契約・地域・番組の配信条件を確認してからHLSと先頭の1セグメントを取得します。

```sh
cargo run -- probe-live TBS
cargo run -- probe-timefree TBS 20261014120000
cargo run -- probe-timefree TBS 20261005120000
```

日時は日本時間の実日付による`YYYYMMDDHHmmss`で、時は00〜23を指定します。指定時刻を含む放送枠全体を解決し、その冒頭のサンプルを確認します。過去の日付は実行時の聴取期限に合わせて選び直してください。`--data-dir DIR`も指定できます。

結果には放送枠、認証エリア、HLS宣言コーデック、サンプル形式、デコード確認の成否を表示します。Cookie・トークン・配信URL・音声データは出力／保存しません。終了コード0はHLSとサンプルの取得成功を表し、デコード成功や連続再生の保証ではありません。

2026年10月15日にOBSの地元ライブ、TBSのエリア外ライブ・前日分・10日前のタイムフリーで取得を確認しました。確認したHLSは`mp4a.40.5`（HE-AAC系）を宣言しており、採用中のSymphonia 0.5.5は完全なHE-AACデコードに未対応です。この場合は未対応と表示し、AACコアだけをデコードして再生対応済みとは扱いません。実音声のSBR/PS解析、音声出力・連続再生・番組末尾の停止は未検証です。

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
