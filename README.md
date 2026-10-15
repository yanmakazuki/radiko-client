# radiko-client

macOS向けRust製radiko TUIクライアントの初期実装です。現在はローカルのチャプターJSONを解析する検証用CLIのみで、ログイン・通信取得・音声再生・TUIは未実装です。

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

[機能設計](docs/design.md) / [アーキテクチャ](docs/architecture.md) / [実装状況と検証の制約](docs/implementation.md)
