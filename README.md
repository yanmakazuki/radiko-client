# radiko-client

macOS向けRust製radiko TUIクライアントの初期実装です。現在は会員ログイン・セッション確認とローカルのチャプターJSON解析の検証用CLIを実装しています。配信取得・音声再生・検索・TUIは未実装です。会員認証の実アカウント検証はまだ行っていません。

```sh
cargo run -- login
cargo run -- session
cargo run -- logout
```

`login`は対話端末でメールアドレスとパスワードを入力します。パスワードは非表示で、保存しません。ログイン後にセッションの有効性を別途確認し、エリアフリー・タイムフリー30の契約情報を表示します。契約情報だけで個別番組が再生可能とは判断しません。

保存するのは再利用可能なセッションのみです。保存先は`directories`のユーザーデータディレクトリ内の`session.json`です。`--data-dir DIR`で変更できます（例：`cargo run -- --data-dir ./local-data login`）。専用ディレクトリを指定してください。Unixではディレクトリ0700・ファイル0600が必要で、新規作成時に設定します。セッションは平文のため、Gitなどで共有しないでください。

`session`は保存済みセッションを自動で読み込み、サーバーで確認します。ファイルがない場合は未ログイン、破損・未対応形式なら理由を表示して元ファイルを保持します。`logout`は通信せずローカルのセッションを削除します（公式Webなど別端末のログインは変更しません）。明示的な`login`が成功したときだけ保存を置き換えます。

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
