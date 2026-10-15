# 実装状況

## 2026年10月15日：会員認証とセッションの検証用入口

追加内容：

- `login`：対話端末でメールアドレスと非表示のパスワードを入力。パスワードは引数・環境変数から受け取らず、永続保存しない。
- `session`：保存済みセッションを読み込み、HTTPで有効性を確認。エリアフリー・タイムフリー30の契約を個別に表示し、不明な項目は未確認とする。個別番組の再生可否は断定しない。
- `logout`：ネットワークに依存せずローカルセッションを削除。公式Webや他端末のセッション失効操作は行わない。
- `--data-dir`：保存先の指定。既定は`directories`のユーザーデータ領域。保存形式はバージョン1のJSONで、再利用可能なセッショントークンのみを保持。
- 原子的な保存、一時ファイルを含むUnixの0700／0600権限、破損・未対応形式の保持、明示的ログイン成功時の置換。シンボリックリンクなど不適切な保存先は拒否。
- reqwestのHTTPS通信に接続10秒／全体20秒のタイムアウト、認証応答64KiBの上限を設定。リダイレクトしない。GETの一時的通信障害・408・5xxのみ最大3回再試行し、指数バックオフとジッターを使用。ログインPOST・失効・403・429は自動再試行しない。429後の自動要求も行わない。
- パスワード・Cookie・トークン・応答本文をエラーへ埋め込まない。セッションのDebug表示は伏せ字にする。

取得先の根拠（同日確認）：

- ログイン候補`POST https://radiko.jp/ap/member/webapi/member/login`と`mail`／`pass`、応答の`radiko_session`は[公開実装](https://github.com/garret1317/yt-dlp-rajiko/blob/master/yt_dlp_plugins/extractor/radiko.py)を参照。
- 有効性確認`GET https://radiko.jp/ap/member/webapi/v2/member/login/check`、成功の`status=200`、`areafree`／`timefreeplus`は[公式Webの公開コード](https://radiko.jp/apps/js/menu.js)も取得して確認。
- reqwest 0.12.28の採用APIは取得済みクレートのソースとコンパイルで確認。採用バージョンはCargo.lockで固定。

検証手順と結果：

| 手順 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | 成功 |
| `SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo clippy --offline --all-targets --all-features -- -D warnings` | 成功、警告なし |
| 同じSDK指定で`cargo test --offline --all-targets --all-features` | 全21件成功（既存6件、認証モック6件、保存5件、CLI4件） |

認証モックでは失敗・失効・契約情報欠落・不正応答・再試行上限を検証。保存では権限、上限、破損／未対応形式の保持、保存中断、置換、削除を確認。CLIはファイルなし・破損時の未ログイン継続、対話端末必須、引数検証、既存のチャプター解析を実プロセスで確認した。通常テストは実アカウント・radikoへの通信に依存しない。

**未検証／未実装**：実アカウントによるログイン、保存セッションを使った実サーバーでの復元、配信認証、ライブ・通常タイムフリー・8〜30日前の配信取得、コンテナ／コーデック確認、音声再生、検索、TUI。会員認証を実証済みとは扱わない。次はユーザーがローカル端末で`login`・`session`を実行して契約認証を確認し、その結果を踏まえて配信取得用の入口を追加する。

## 2026年10月15日：ローカル実装の再検証

対象は現行の放送日・放送状態判定、チャプター解析と検証用CLI。fmt導入後に`cargo fmt --all`で整形を適用した。動作の変更は行っていない。

| 手順 | 結果 |
|---|---|
| `cargo test --all-targets --all-features` | 制限内では名前解決に失敗。制限外で依存取得・Cargo.lock生成に成功したが、既定のMacOSX27.0 SDKの`arm64e.x1`をリンカーが解釈できず失敗 |
| `SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo test --offline --all-targets --all-features` | ビルド成功、既存の結合テスト6件すべて成功 |
| 同じ`SDKROOT`で`cargo build --offline --all-targets --all-features` | 成功 |
| ビルド済みCLIを実行し、終了コードと出力を確認 | 9ケース成功：ヘルプ、fixture通常解析、分割枠補正、長さ0、NaN補正、不正コマンド、空配列、不正JSON、1MiB超過 |
| `cargo fmt --all -- --check` | 導入後の初回は整形差分で失敗。`cargo fmt --all`適用後は成功 |
| 同じ`SDKROOT`で`cargo clippy --offline --all-targets --all-features -- -D warnings` | 導入後に成功、警告なし |

Rust/Cargoは1.98.1/1.98.0。最低対応バージョン1.85での実行は未確認。Cargo.lockを追加し、生成物`target/`はGit管理対象外とした。SDK指定はコマンド単位の回避策であり、システムの設定は変更していない。

ローカル実装の検証範囲では不具合は再現しなかった。ログイン・HTTP取得・音声再生・検索・TUIは未実装のため、実アカウント・実通信・30分連続再生は未検証。整形適用後にfmt・clippy・既存テスト6件を再実行し、すべて成功した。

## 2026年10月15日：初期実装

追加した内容：

- 単一Cargoプロジェクトとライブラリ／CLIの入口。
- 日本時間の午前5時区切りの放送日計算、放送開始・終了境界の状態判定、放送枠の識別型。
- チャプターJSON解析。レスポンス上限、必須配列、楽曲除外、タイトルの優先順位、制御文字除去、時刻補正と枠外項目の除外、安定ソート、重複・隙間の現在チャプター判定。
- ローカルJSONの解析用`inspect-chapters`コマンド。対応付け・認証・聴取可否は推測しない。
- 合成JSON fixtureと、日付境界・放送状態・分割枠補正・不正構造・重複判定の6つのテスト。

検証手順と結果：

| 手順 | 結果 |
|---|---|
| `rustc --version` / `cargo --version` | 1.98.1 / 1.98.0を確認 |
| `cargo test --all-targets --all-features` | crates.ioの名前解決失敗で依存取得できず、コンパイル・テスト未実施 |
| `cargo fmt --all -- --check` | cargo-fmt未導入で未実施 |
| `cargo clippy --all-targets --all-features -- -D warnings` | cargo-clippy未導入で未実施 |

依存APIはchronoとserde_jsonの公式ドキュメントを参照したが、採用バージョンでのコンパイル確認は未完了。Cargo.lockは依存取得ができた時点で生成して管理する。現時点でビルド成功とは扱わない。

次に行うこと：まず依存取得と所定チェックを完了する。その後、開発順序に従い、ローカルで入力する認証情報を用いた検証用入口を追加し、契約認証・配信取得・実コーデックを検証する。番組表HTTP/XML取得、チャプターHTTP取得とeventId解決、HLSと音声出力、検索・保存、非同期アプリ状態とTUIは未実装。実アカウントやradikoへの通信検証は今回行っていない。
