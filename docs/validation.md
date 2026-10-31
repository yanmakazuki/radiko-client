# 検証範囲

2026年10月31日時点で、何を確認し、何が残っているかの要約です。日付、コミット、フレーム数、手順は [実装状況](implementation.md) が正です。古い節の「未実装」「未接続」は、その日の状態なので、このページの表と読み比べてください。

仕様は [機能設計](design.md)、処理の上限は [アーキテクチャ](architecture.md) です。

## 確認できたこと

再生の長時間確認はミュートが中心です。耳で聴いたと書いてあるもの以外は、聴こえ方の確認ではありません。30分は、同じ出力世代で1800秒以上、検索の確定とチャプター要約の切り替えを含む操作に応答し、終了コード0と端末の復元までを見たものを指します。ビルドが違う結果は足し合わせません。

| 項目 | 確認できた範囲 | 残っていること |
| --- | --- | --- |
| 番組検索 | 単局、地域、全国。単日から最大30日。分類とキーワード。`e` で失敗と不正枠を表示。再生を続けたまま、全国30日・132,614枠の一部取得が84.79秒で完了（地域と局日の失敗0、不正4）。ヘルプの応答は最大272ms | その取得は全枠が有効という意味ではない。起動時に30日分は取らない |
| 条件の編集と保存 | 確定まで通信しない編集、名称の複数選択、名前付き条件、起動時は日付だけ当日に戻す。通常テストと疑似端末 | 実通信で全操作の組み合わせを網羅したわけではない |
| 認証 | CLI と TUI のログインをユーザーが確認。保存の権限 `0700`/`0600`、破損ファイルを残すこと、ログアウト。再生中の HTTP 401 は、プロセス内のモックと `127.0.0.1` の HTTP サーバーの両方で1回再認証して継続し、再認証失敗と連続 401 は「認証期限切れのため再生を停止しました。aで再確認するか、loginで再ログインしてください」で停止する。429 と 403 は再認証しない。401・403・429 はローカルの HTTP 応答から対応するエラーになる | すべての契約と、通信失敗の全種類を網羅したわけではない。実配信のトークン期限切れと、90分以上のライブでの復旧は未確認 |
| 通常のタイムフリー | TBS の12時枠で1800秒超、57操作、検索確定7回、章要約5回、最終の不足0、再再生約5秒、終了0、端末復元。複数の固定ビルド | 聴こえ方、全局、全形式の保証ではない |
| 8〜30日前 | 29日前と30日前の TBS、および13〜17日前の TBS で、上と同じ種類の30分。不足0 | 聴取期限の時刻ぴったりは未確認 |
| ライブ | FMJ で1800秒超の指定時間停止。RN1 の短い枠で自然終了し、次の枠へ進まない。別の USB 出力25秒は、ユーザーが自然で、音切れと雑音は気にならないと回答。配信窓が先に進んだときの追いつき、1つ古いプレイリストを待って取り直さないこと、窓の長さを超える飛びをリセットにすることは、偽のプレイリストだけで確認。poll が1秒より長く空いたあとの復帰と、30秒を超える通信断の再接続も、注入した時計だけで確認。バッファが空いたあとの「受信待ち…」と、30秒での「再接続中…（N秒）」への切り替えも、注入した時計と偽のプレイリストだけで確認。ライブとタイムフリーの両方。実配信の22秒・60秒の通信断は未確認 | 自然終了、聴こえ方、機器切断のあとの長時間を、同じ結果には含めない。配信窓が先に進む実配信での追いつき、実機のスリープ復帰、実配信の通信断は未確認 |
| シークと枠の切り替え | 秒指定、15秒、同じ枠と別枠の章、末尾停止。ライブから章への切り替え、停止中の位置保持と再開 | 全経路と全局ではない |
| チャプター | 公開メタデータは、確認した日の対象では原点が枠の開始と一致。TBS の二枠は、交通情報の開始秒をユーザーが聴いて対応を確認。原点が一致する章だけジャンプ | 原点が違う実例はなく、補正はない。全放送日と全章の境界精度ではない |
| 取り消しと終了 | HTTP の待ちと本文、AAC のパケット前後、部分データの破棄。SIGINT、SIGTERM、SIGHUP で端末復元。終了時は、応答しないローカル HTTP の途中でも出力の破棄が1.5秒以内に返り、準備が5秒ブロックしても同じ。通常画面の Esc は終了せず、出ている通知を消す。再生中のスナップショットと停止要求は変わらない（ユニットテスト） | OS の名前解決と、1パケットの復号呼び出し自体は途中で切れない。SIGKILL と、認証の書き込み中の完了は保証しない。再生中の `q` の直後に実機の `output-info` が0になること、実機の TUI で Esc を押した聴感は未確認 |
| 出力とコーデック | 純 Rust の AAC/SBR と CPAL。既定と同じ名前の機器を開く。MacBook Air Speakers の無音3秒は不足0。Yamaha URX22 A は無音のあと物理切断で機器エラーになり、接ぎ直し後の無音5秒は不足0。JP1〜JP47 の96局のライブ先頭は `mp4a.40.5` の ADTS で SBR。Linux のカードなしでは「既定の音声出力機器がありません」の1行で、`ALSA lib` も JACK の行も 0 である。`RADIKO_ALSA_LOG=1` のときは両方のログが残る。PulseAudio の null sink では `default` の f32・48000Hz・2ch で無音3秒、不足0 | TUI 操作中の切断、失敗した再生の自動再開、他の機器は未確認。実配信の非 ADTS と、PS の明示が真になる例は未確認。Linux の実機再生、i16 だけの機器を開いた再生、PipeWire のデスクトップは未確認 |

2026年10月31日、通知がない通常画面の Esc は「終了は q（または Ctrl+C）」を出し、もう一度 Esc で消す。出ている通知は消すだけで案内に替えない。停止文を消したあとに「停止中」とその理由は残らない。Linux の Rust 1.99.1（58cf8f9c3、2026-10-13）と Rust 1.85.0（4d91de4e4、2025-02-17）で、`cargo fmt --all -- --check`、`cargo clippy --workspace --locked --all-targets --all-features -- -D warnings`、`cargo test --workspace --locked --all-targets --all-features -- --test-threads=8` はどちらも 410 件成功、失敗 0、ignored 2 件である。Python は 75 件成功である。実機の再生と radiko への通信はない。詳細は [実装状況](implementation.md) の同日の節である。以下は、その前の再実行である。2026年10月31日、この変更を含む作業ツリー（main の `897f69d` に、画面内ヘルプの1文と文書の更新を足したもの）を Linux x86_64 で再実行した。Rust 1.99.1（stable、`58cf8f9c3`、2026-10-13）と Rust 1.85.0（`4d91de4e4`、2025-02-17）のどちらも、`cargo test --workspace --locked --all-targets --all-features -- --test-threads=8` は 404 件成功、失敗 0、ignored 2 件である。件数は各 `test result` 行の成功数を足したものである。同じ日の `python3 -m unittest discover -s scripts -p 'test_*.py'` は 58 件成功である。`cargo fmt --all -- --check` と `cargo clippy --workspace --locked --all-targets --all-features -- -D warnings` も、両方のツールチェインで成功した。Checks の run 40865375776 は、`897f69d` に対して 4 ジョブすべて success である。API の `run_started_at` は 2026-10-29T11:26:12Z、`updated_at` は 2026-10-29T11:29:07Z で、日本時間（JST、UTC+9）では 2026-10-29 20:26:12〜20:29:07 である。ジョブは Format, lint, and tests（ubuntu-24.04 と macos-15）と Rust 1.85.0（ubuntu-24.04 と macos-15）である。同じ run のログで `test result: ok` の成功件数をジョブごとに足すと、ubuntu-24.04 の stable と Rust 1.85.0 はどちらも 404 件成功、失敗 0、ignored 2 件である。macos-15 の stable と Rust 1.85.0 はどちらも 395 件成功、失敗 0、ignored 2 件である。Format, lint, and tests のログでは、`tests/cli.rs` が ubuntu-24.04 の 17 件に対し macos-15 は 16 件、`tests/full_device.rs` が 3 件と 0 件、最初の `test result`（`player::` などを含む）が 240 件と 235 件である。その 5 件のテスト名は突き止めていない。Rust 1.85.0 のジョブは合計だけを足し、ファイルごとの内訳は見ていない。Python は ubuntu-24.04 が `Ran 58 tests` の OK、macos-15 が `Ran 58 tests` の `OK (skipped=1)` である。Rust 1.85.0 のジョブは Python を実行しない。以下はそれより前の記録である。自動テストは、2026年10月24日の通常 Rust で305件が成功しています。ミュートの実機4件は ignored のままです。fmt と clippy も成功しています。Rust 1.85 での、その日の再ビルドはしていません。それより前の変更では、Rust 1.85.0 のオフラインテストが成功した記録があります。GitHub Actions は、fragmented AAC より前のコミットで成功を確認し、10月24日の `0f419e5` の Checks はこの要約の時点では未確認です。2026年10月26日に ubuntu-24.04 の同じ検査をワークフローへ足しました。GitHub 上のその実行はまだありません。同日の macOS では、通常 Rust 1.98.1 で fmt、clippy、306件が成功し、ignored 4件はそのままです。同日の Linux、通常 Rust 1.99.1 では、終了時に出力を先に閉じる確認で fmt、clippy、308件が成功し、ignored 4件はそのままです。この確認には音声デバイスがなく、再生と端末の操作確認はしていません。別の確認では、同じ Linux の Rust 1.99.1 で fmt、clippy、312件が成功し、ignored 4件はそのままです。カードなしと PulseAudio の null sink の出力確認は [実装状況](implementation.md) の同日の節です。実機の再生ではありません。macOS の再生結果は Linux の結果にしません。同日の再生中 401 の変更は、Linux の Rust 1.99.1 で fmt、clippy、318件が成功し、ignored 4件はそのままです。モック上の再認証は含みます。radiko の実トークン期限切れと、90分以上のライブでの復旧は、日本からの接続がないため未確認です。2026年10月27日にその変更へ origin/main を取り込み、Linux の Rust 1.99.1 で fmt、clippy、327件、Rust 1.85.0 でテスト 327件が成功しました。どちらも失敗 0、ignored 4件です。同日、`ProbeError` の表示を全バリアントの網羅的な分岐にして `unreachable!` を外しました。ローカル HTTP のタイムアウト、再試行、401・403・429 の対応、401 のあとの再認証は、通常の `cargo test` に入ります。`native_muted` の 2 件は ignored のままです。この確認は、Linux の Rust 1.99.1 で fmt、clippy、332件、Rust 1.85.0 でテスト 332件が成功し、どちらも失敗 0、ignored 2件です。実配信のトークン期限切れは、この自動テストでは確認していません。同日、ライブの `MEDIA-SEQUENCE` が窓より先へ進んだときと巻き戻ったときの追いつきを足し、Linux の Rust 1.99.1 で fmt、clippy、333件、Rust 1.85.0 でテスト 333件が成功しました。どちらも失敗 0、ignored 4件です。実配信で窓が先に進む経路は確認していません。同日、CLI の標準出力が閉じたパイプへ書いたときのパニックを、`BrokenPipe` を終了コード 141 で戻すように直しました。Linux の Rust 1.99.1 で fmt、clippy、332件、Rust 1.85.0 でテスト 332件が成功しました。どちらも失敗 0、ignored 4件です。`programs --areas JP13 | head -1` は終了 141、panic なし、stderr は進捗2行のままです。詳細は [実装状況](implementation.md) の同日の節です。同日、その変更を配信窓の追いつきへ merge し、Linux の Rust 1.99.1 で fmt、clippy、338件、Rust 1.85.0 でテスト 338件が成功しました。どちらも失敗 0、ignored 4件です。追いつきの表示は stderr の通信診断のままで、標準出力は `stdout_ln!` のままです。同日、小さな巻き戻りは次の取得を待ち、窓のセグメント数を超える飛びはリセットにするよう直し、Linux の Rust 1.99.1 で fmt、clippy、340件、Rust 1.85.0 でテスト 340件が成功しました。どちらも失敗 0、ignored 4件です。タイムフリーの欠落と巻き戻りは、これまでと同じ失敗のままです。同日、窓の先頭からの再開通知を「配信窓の先頭から再開しました（累計N）」にし、同じ 340 件が再び成功しました。ライブ端の通知は変えていません。同日、少し古いライブ窓は末尾が最後に再生した番号からセグメント数以内のときだけ無視するよう直し、Linux の Rust 1.99.1 で fmt、clippy、343件、Rust 1.85.0 でテスト 343件が成功しました。どちらも失敗 0、ignored 4件です。それより前の末尾と、大きく小さい番号への切り替わりはリセットのままです。同日、スリープ復帰と30秒を超える通信断を、再生を終えずに5分まで再接続するよう直し、Linux の Rust 1.99.1 で fmt、clippy、355件、Rust 1.85.0 でテスト 355件が成功しました。どちらも失敗 0、ignored 4件です。上限を過ぎたときだけ「再接続の上限を超えたため再生を停止しました。番組を選び直して再生してください」で止まり、再接続中は「再接続中…（N秒）」です。実機のスリープ復帰と、実配信の通信断は未確認です。同日、タイムフリーで未取得部分が配信窓から外れた停止を `WindowLost` にし、文言は yanma さん確認済みの「通信が長く途切れたため、続きを再生できなくなりました。番組を選び直してください」です。Linux の Rust 1.99.1 で fmt、clippy、356件、Rust 1.85.0 でテスト 356件が成功しました。どちらも失敗 0、ignored 4件です。巻き戻りはプレイリスト不正のままで、先へは飛ばしません。2026年10月28日、ライブで失効した chunklist の HTTP 404 を、最初の配信 URL からの取り直しにしました。Linux の Rust 1.99.1 で fmt、clippy、360件、Rust 1.85.0 でテスト 360件が成功しました。どちらも失敗 0、ignored 4件です。404 が続くときは再接続の上限文言で止まり、タイムフリーの 404 は `NotFound` のままです。実配信の STOP/CONT は未確認です。同日、その変更へこのブランチを merge し、`ProbeError` の表示は `WindowLost` と `ReconnectGaveUp` を含む網羅的な分岐のまま、`unreachable!` はないままにしました。ローカル HTTP の 4 件は通常テストのままです。Linux の Rust 1.99.1 で fmt、clippy、365件、Rust 1.85.0 で fmt、clippy、テスト 365件が成功しました。どちらも失敗 0、ignored 2件です。同日、`scripts/verify_tui_playback.py` は、親が SIGINT を無視していても SIGINT で一時ディレクトリを消し、終了コード 130 で戻る。radiko への接続は使わず、通常のシェルと `trap '' INT` の両方で Python のテスト 51 件が成功した。Rust のテストは足していない。Linux の Rust 1.99.1 で `cargo test --workspace --all-targets --all-features` は 365 件成功、失敗 0、ignored 2 件です。実機の再生は確認していない。同日、`tui --offline` を足し、ubuntu-24.04 の Format, lint, and tests が疑似端末で操作一覧、検索条件、取得状況、ライブ画面を切り替えて `q` で終了することを確認します。macOS のジョブと Rust 1.85.0 のジョブにはこの手順はありません。Linux の Rust 1.99.1 で fmt、clippy、369件、Rust 1.85.0 で clippy とテスト 369件が成功しました。どちらも失敗 0、ignored 2件です。同じビルドのスモークは 10 回連続で終了 0、端末復元、`ALSA lib` とパニックなし、TCP/UDP なしでした。radiko への通信と実機の再生はありません。その後、clap は外し、`--offline` はヘルプに書かないだけにした。セッションと検索条件のファイルはオフラインでは読まない、書かない、消さない。ubuntu-24.04 のスモークは `unshare -rn` の中で、先に `1.1.1.1:443` へ届かないことを見てから実行する。AppArmor がユーザ名前空間を拒むときは `kernel.apparmor_restrict_unprivileged_userns=0` にしてから同じ `unshare -rn` を使う。Linux の Rust 1.99.1 で fmt、clippy、370件、Rust 1.85.0 で clippy とテスト 370件が成功しました。どちらも失敗 0、ignored 2件です。名前空間の中のスモークは終了 0、端末復元でした。同日、その変更と、親が SIGINT を無視していても検証スクリプトが中断する変更を merge しました。コードの衝突はなく、記録だけを両方残しました。Linux の Rust 1.99.1 で fmt、clippy、370件が成功し、失敗 0、ignored 2件です。Python のテストは通常のシェルと `trap '' INT` の両方で 56 件成功しました。Rust 1.85.0 はこの確認では再ビルドしていません。同日、終了キーの表を足し、画面内ヘルプと `tui --help` の終了とログアウト確認の文言を同じ句にした。Linux の Rust 1.99.1 で fmt、clippy、371件、Rust 1.85.0 でテスト 371件が成功しました。どちらも失敗 0、ignored 2件です。Python のテストは 56 件成功です。radiko への通信と実機の再生はありません。同日、ダイアログの `q` は閉じるだけに揃えた。ログアウト確認の `q` は取消で、ログアウトは実行しない。同じ 371 件が Rust 1.99.1 と 1.85.0 で再び成功し、失敗 0、ignored 2件です。Python は 56 件成功です。`unshare -rn` のオフライン TUI スモークは終了 0、端末復元でした。同日、窓落ち、再接続の上限、認証期限切れの停止文は、TUIの再生通知とCLIのエラー行ではその文だけを出す。「再生停止:」「配信取得:」と段階名は付けない。他の失敗の連鎖は変えていない。停止したあとの「再接続中…」と追いつきは、文面ではなく停止フラグで止める。種類と段階名はCLIの標準エラーの次の行に残す。Linux の Rust 1.99.1 で fmt、clippy、375件、Rust 1.85.0 で clippy とテスト 375件が成功しました。どちらも失敗 0、ignored 2件です。Python は 57 件成功です。`unshare -rn` のオフライン TUI スモークは終了 0、端末復元でした。`--help` は main と同じです。radiko への通信と実機の再生はありません。同日、バッファが空いた無音が2秒続いたら TUI に「受信待ち…」を出し、30秒で「再接続中…（N秒）」へ切り替えるようにした。2秒未満は出さない。一時停止、停止フラグ、最初の音声の前、番組終了では出さず、確認済みの停止文と追いつきは置き換えない。CLIの状態行には出さない。ライブとタイムフリーは、注入した時計と偽のプレイリストで、22秒の途絶は「受信待ち…」だけで、60秒の途絶は30秒目に切り替わる。Linux の Rust 1.99.1 で fmt、clippy、379件、Rust 1.85.0 で clippy とテスト 379件が成功しました。どちらも失敗 0、ignored 2件です。Python は 57 件成功です。`unshare -rn` のオフライン TUI スモークは終了 0、端末復元でした。`--help` とオフライン画面は変更前とバイト単位で同じです。実配信の22秒・60秒の通信断は未確認です。そのあと、空の再生更新で「受信待ち…」と「再接続中…」を消さないようにし、追いつきと配信認証の文は5秒だけ守って、そのあと無音なら置き換えるようにした。操作の結果（シーク不可、切替不可、再生準備失敗）も同じ5秒である。Linux の Rust 1.99.1 で fmt、clippy、382件、Rust 1.85.0 でテスト 382件が成功しました。どちらも失敗 0、ignored 2件です。Python は 57 件成功です。`unshare -rn` のオフライン TUI スモークは終了 0、端末復元でした。`--help` とオフライン画面は変更前とバイト単位で同じです。そのあと、保持する通知を種類ごとに4.9秒と5.1秒で見るようにし、番組終了の抑止は5秒の保持が切れたあとの無音で見るようにした。表示は変えていない。Linux の Rust 1.99.1 で fmt、clippy、383件、Rust 1.85.0 で clippy とテスト 383件が成功しました。どちらも失敗 0、ignored 2件です。Python は 57 件成功です。`unshare -rn` のオフライン TUI スモークは終了 0、端末復元でした。`--help` とオフライン画面は変更前とバイト単位で同じです。同日の夜から翌日にかけて、閉じた標準エラーへ書いてもパニックせず、終了コードを変えないようにした。書けない表示は捨てる。Linux の Rust 1.99.1 で fmt、clippy、389件、Rust 1.85.0 で fmt、clippy、テスト 389件が成功しました。どちらも失敗 0、ignored 2件です。Python は 57 件成功です。`unshare -rn` のオフライン TUI スモークは終了 0、端末復元でした。標準エラーを開いた CLI は `568e130` とバイト列が同じです。そのあと、標準出力の空き容量不足では残りの番組表を止めて終了コード 1 とし、「エラー: 書き込みに失敗しました（空き容量不足）」を一行出すようにした。閉じた標準出力は 141 のままである。標準エラーが書けないだけでは取得を止めない。`/dev/full` の試験は Linux だけである。Linux の Rust 1.99.1 と Rust 1.85.0 で fmt、clippy、テスト 395 件が成功し、失敗 0、ignored 2 件である。Python は 57 件成功である。`unshare -rn` のオフライン TUI スモークは終了 0、端末復元だった。radiko への通信と実機の再生はありません。2026年10月29日、機器がなく libjack がある Linux で、列挙と出力オープンのあいだだけ fd 2 を外し、`output-info` と `probe-output 1` の stderr をアプリの1行だけにした。`RADIKO_ALSA_LOG=1` の stderr は変更前とバイト単位で同じである。Linux の Rust 1.99.1 と Rust 1.85.0 で fmt、clippy、テスト 404 件が成功し、失敗 0、ignored 2 件である。Python は 58 件成功である。実機の Q2-N は未確認である。

## まだ確認していないこと

次は、実例や操作が揃ったときに確認します。確認できるまで、対応する実装は足しません。

1. 実配信の非 ADTS。見つかったら、fragmented MP4 の `tfdt`、初期化情報、連続したデコーダー状態と HLS の対応を見る。96局のライブ先頭と、確認した TBS の前日枠では ISO BMFF は出ていません。生成音声の長時間ミュートと、ADTS をメモリ上で詰め直した PCM の一致は別の確認です。
2. TUI を操作しているときの出力機器の切断。Yamaha の切断は無音プローブの1台です。自動再開と、機器を選ぶ機能は入れません。
3. イベント原点が枠の開始と違う番組。96局の2026年10月18日4,242枠と、10月24日4,294枠はすべて原点が一致していました。全放送日を見たわけではありません。
4. 聴こえ方。ユーザーが聴いたと記録した範囲だけを、その確認にします。ADTS のビットストリーム上の PS は、採用デコーダーの公開 API からは読めません。
5. Linux の再生で、実機の操作として残っているもの。確認済みと未確認は、下の「Linux の実機検証」の表と同じである。確認済み: GitHub Actions の CI、ubuntu-24.04 の `unshare -rn` による `tui --offline` 疑似端末スモーク、機器なしの `output-info` と `probe-output`、PulseAudio の null sink、yanma さんの QEMU 仮想マシンでの CLI `play-live` 90分と再生中の終了。未確認: 実機での TUI 再生（同じ仮想マシンの TUI 再生も表では未確認）、Ubuntu 24.04 デスクトップの PipeWire、i16 だけの機器を開いた再生、機器の抜き差し、スリープ復帰。表の `hw:0,0` は 1 秒の `probe-output` が i16 で成功している。i16 だけを提供する機器の再生として、それ以外は未確認のままである。

## 自動テストと明示実行

通常の `cargo test --workspace` は、アカウント、radiko、音声の保存を使いません。次は明示的に実行します。

`native_muted_*` の 2 件は `#[ignore]` のまま、この通常実行には入りません。音声のデコードから出力までの経路、`output-info` と `probe-output`、その判定スクリプト、または `cpal` などの依存を変えた pull request では、ubuntu-24.04 の別ワークフローが PulseAudio の `module-null-sink` で `output-info`、`probe-output --mute 3`、`cargo test --locked -p radiko --lib native_muted -- --ignored --test-threads=1` を実行します。main への push と定期実行では動きません。必要なときは `workflow_dispatch` で手動かします。ドキュメントだけの変更では動きません。ALSA の `default` は `libasound2-plugins` の pulse プラグイン経由で、その null sink に繋がります。ALSA の `null` PCM は実時間で進まないので、このジョブでは使いません。`RADIKO_FRAGMENT_SECONDS` は付けません。fragment のテストは省略時の 5 秒です。取得待ちの切り分けも同じジョブで走ります。`output-info`、`probe-output`、ignored の `native_muted` の stdout と stderr に `ALSA lib` または JACK の行が 1 行でもあるときは、ジョブが失敗します。`probe-output` は、表示された不足が 0 でないときも失敗します。取得待ちの `gap_after=1` は、テストが不足の発生を確認する側なので、0 にはしません。`scripts/check_null_sink_audio.py native` は、ignored の `native_muted` が 2 件成功、失敗 0、ignored 0 でないとき失敗します。`synthetic_fragmented_muted` の不足が 0 でないとき、および `gap_after=4` の不足が 0 でないときも失敗します。

生成した fragmented AAC を、音量0で実機へ出します。通信とアカウントは使いません。`RADIKO_FRAGMENT_SECONDS` は1〜1800で、省略時は5秒です。実時間は指定秒数に30秒を足したところで打ち切ります。

```sh
RADIKO_FRAGMENT_SECONDS=1800 cargo test --workspace --locked -p radiko --lib \
  native_muted_fragmented_aac_reaches_the_explicit_output_limit -- --ignored --nocapture
```

取得待ちと先読みの切り分けは、生成 AAC・音量0で約50秒です。同じテストは上の ubuntu-24.04 ジョブでも走ります。

```sh
cargo test --workspace -p radiko --lib native_muted_acquisition_gap -- --ignored --nocapture
```

ローカルの HTTP は `127.0.0.1` の空いているポートで待ち受け、通常の `cargo test` に入ります。外のホストへは接続しません。タイムアウト、途中で切れた本文の再試行、404、401・403・429 の対応、401 のあとに再認証して再生を続ける経路を含みます。待ち時間は、応答しない接続の 250ms が2回で、1回あたり約0.5秒です。ポートは OS が割り当てるので、通常の並列実行でも衝突しません。音声の null sink ジョブには入れません。通常実行から外しているのは `native_muted` の 2 件だけです。

```sh
cargo test --workspace -p radiko --lib local_mock_http
```

検証スクリプトの画面読み取りは、アカウントも音声も使わずに確認できます。

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
```

## 実機の TUI 検証

`scripts/verify_tui_playback.py` は Python 3 の標準ライブラリと Unix の termios を使います。実機での実行は macOS だけで、Linux では実行していません。実行ファイルとセッションを、権限を制限した一時コピーにしてから使います。音声はミュートです。子プロセスは親の制御端末から分離し、指定した PTY の寸法も見ます。番組本文、音声、URL、Cookie、トークンは残しません。通常の `cargo test` からは呼びません。

事前にビルドし、有効な保存セッションと、十分な長さの枠を指定します。日付と枠は、そのとき聴けるものへ変えてください。

macOS のセッションは `~/Library/Application Support/jp.radiko-client.radiko-client/session.json` です。Linux では `$XDG_DATA_HOME/radiko-client/session.json` です。`XDG_DATA_HOME` がなければ `~/.local/share/radiko-client/session.json` です。

macOS:

```sh
python3 scripts/verify_tui_playback.py \
  --session-file "$HOME/Library/Application Support/jp.radiko-client.radiko-client/session.json" \
  --station TBS --date 20261015 --start 20261015120000
```

Linux:

```sh
python3 scripts/verify_tui_playback.py \
  --session-file "${XDG_DATA_HOME:-$HOME/.local/share}/radiko-client/session.json" \
  --station TBS --date 20261015 --start 20261015120000
```

よく使う指定は次です。

| 目的 | 指定 |
| --- | --- |
| 短時間の操作 | `--seconds 60 --interval 3` |
| 指定位置から | `--offset 1000 --seconds 120`。タイムフリー |
| 30分止めてから再開 | `--pause-seconds 1800 --seconds 30`。停止時間は出力秒数に含めない |
| 枠の末尾 | `--timefree-end --offset 3590 --seconds 15`。1時間枠の残り10秒。残りは5秒以上かつ `--seconds` 以内。`mode=timefree_end` |
| ライブを指定秒数で止める | `--live --station FMJ --date auto --start auto --seconds 60`。`mode=live`。自然終了ではない |
| ライブの自然終了 | `--live-end --date auto --start auto --station RN1`。終了まで30秒超、指定秒数以内の放送中枠。`mode=live_end` |
| 測定のあと冒頭から再生し直し | `--restart-check`。タイムフリー。本測定と再再生の秒数は別 |
| スリープと復帰 | `--sleep-check --seconds 30`。操作者がスリープする。`sleep_ready` のあと復帰し、`awake` と Enter。待ちは10分 |
| 外付け機器の切断 | `--device-check --seconds 30`。`device_ready` の表示に合わせて切断と再接続を入力する。OS の出力先はスクリプトから変えない |

`--live-end`、`--timefree-end`、`--sleep-check`、`--device-check`、`--pause-seconds` は互いに併用しません。末尾と自然終了の最後の10秒は、画面を開かずに終了を待ち、止まった状態を3秒維持することを見ます。停止キーは送りません。

`starting` は再生を要求した試行、`playing` は要求した枠とモードで出力秒数が正になった状態です。出力0の停止は開始成功にしません。終了 JSON には完了、不足、端末復元、ビルドのハッシュを出します。Ctrl+C、SIGTERM、SIGHUP で中断しても、検証プロセスと一時ファイルを片付けます。

画面のスリープを検証中だけ止めるときは、macOS ではコマンドの先頭に `caffeinate -i` を付けます。Linux では `systemd-inhibit --what=sleep:idle` を同じ位置に付けます。プロセスが終わると解除されます。記録にはその条件を残し、スリープ復帰の確認とは分けます。

不足が増えた読取は最大8件まで通知し、回数と直近1件を残します。操作中は読取の間隔が伸びるので、音切れの回数や原因の断定には使いません。処理診断と通信診断はワーカーの直近の観測で、停止後に全通信を集計したものではありません。

全国検索の負荷は、アカウントを使わない別スクリプトです。取得件数、時間、失敗、不正枠、子プロセスのピーク RSS、ヘルプまでの時間、端末復元を出します。PTY の読取待ちを含むので、描画だけの計測ではありません。`catalog_complete` の `peak_child_rss_bytes` の単位はバイトです。macOS の `ru_maxrss` はバイトなので、そのまま出します。Linux の `ru_maxrss` は KiB なので、1024 倍してバイトに揃えます。

```sh
python3 scripts/verify_tui_catalog.py --start 20260921 --end 20261020
```

`--station TBS` で単局にできます。音声再生はしません。

TUI の起動確認は、アカウントも radiko も使わない別スクリプトです。デバッグビルドのあと、作業ディレクトリで次を実行します。事前に `pkg-config` と ALSA の開発ファイル（Debian と Ubuntu では `libasound2-dev`）が入っている必要があります。Python は標準ライブラリだけで、疑似端末は Unix のものです。

```sh
cargo build --locked --bin radiko-client
python3 scripts/smoke_tui.py --binary target/debug/radiko-client
```

40 行 160 列の疑似端末で `tui --offline TBS` を起動し、操作一覧、検索条件、取得状況、ライブ画面を切り替えて `q` で終了します。終了コード 0、端末の復元、出力に `ALSA lib` と `panicked` がないこと、プロセスが TCP/UDP を開いていないことを見ます。`--offline` はテスト専用で、ヘルプには出ません。

ネットワークのない名前空間で同じ確認をするときは、先にホストから `1.1.1.1:443` へ届くことを見て、そのあと名前空間の中では届かないことを見てからスモークします。`unshare -rn` が `Operation not permitted` になる Ubuntu 24.04 では、AppArmor が権限のないユーザ名前空間を止めているので、次の sysctl のあとにもう一度 `unshare -rn` を使います。スモーク自体はホストの root では動かしません。

```sh
python3 -c 'import socket; socket.create_connection(("1.1.1.1", 443), timeout=5).close()'
unshare -rn true || sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
unshare -rn python3 scripts/smoke_tui.py --require-isolated-network --binary target/debug/radiko-client
```

ローカルスモークで sysctl を 0 にしたあとは、`sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=1` で AppArmor の制限を戻します。

`--require-isolated-network` は、同じプロセスから `1.1.1.1:443` へ TCP 接続できたときに失敗します。接続できなければ、続けて疑似端末の確認をします。CI の ubuntu-24.04 は、この `unshare -rn` の形です。

## Linux の実機検証

Linux で、どの環境と音声サーバーで何を確認したかの記録です（#26）。手順は QA の手動テスト計画（Q1〜Q6）に沿います。yanma さんの Linux マシンは QEMU の仮想マシンで、物理 PC ではありません。出力機器は QEMU の仮想 HDA Intel（ハードウェアとしては S16_LE・2ch・16000〜96000Hz）です。本物の USB DAC や内蔵サウンドカードの結果ではありません。音は耳では聴いておらず、モニターの録音を解析して判定しています（再生中の終了だけはユーザーの操作で確認）。macOS の結果は、ここに含めません。

状態は次の3つです。

- 確認済み: その環境で実行し、結果を記録したもの。括弧内は確認した範囲です。
- CI のみ: GitHub Actions でビルドとテストを通したもの。ubuntu-24.04 の Format, lint, and tests は、加えて `tui --offline` の疑似端末スモークを実行します。音声出力と radiko への通信は含みません。
- 未確認: まだ実行していないもの。

### マトリクス

| 環境 | PipeWire | PulseAudio のみ | 素の ALSA | 機器なし |
| --- | --- | --- | --- | --- |
| yanma さんの Linux（QEMU 仮想マシン、仮想 HDA Intel。ディストリビューションは未記録） | 確認済み（`output-info`、`probe-output 1`、未ログインの `tui` 起動と終了）2026-10-27 `cf935eb`。確認済み（CLI の `play-live` 90分）2026-10-27、#33 のビルド。確認済み（再生中の終了）2026-10-27、#32 のビルド。TUI での再生は未確認 | 未確認 | 確認済み（`hw:0,0` で `output-info`、`probe-output 1`、44100Hz 固定時のエラー）2026-10-27 `cf935eb`。`play-live` は未確認 | 未確認 |
| Ubuntu 24.04 デスクトップ（物理 PC） | 未確認（Q1） | 未確認（Q2） | 未確認（Q2） | 未確認 |
| Ubuntu 24.04（GitHub Actions ランナー） | — | — | — | CI のみ（fmt、clippy、Rust と Python のテスト、Rust 1.85.0 のテスト。2026-10-28 から ubuntu の stable ジョブに `tui --offline` の疑似端末スモーク） |
| Debian 13（QA のコンテナ、`/dev/snd` なし） | 未確認 | 確認済み（PulseAudio 17.0 の null sink。実機ではない）2026-10-27 `a5b8786` | 未確認（ALSA の `null` PCM は実時間で進まないため、検証に使えない） | 確認済み（`output-info`、`probe-output 1`）2026-10-27 `a5b8786` |

- #33 のビルドは、再生の時刻（18:00 JST 終了）から見て `cd1c8ad`、#32 のビルドは `fcf13f7` と考えられますが、実行時にコミットを記録していません。次回からは `git rev-parse --short HEAD` を記録します。
- PulseAudio のみ、USB DAC の抜き差し、スリープ復帰、物理 PC での機器なしは、どの環境でも未確認です。

### 結果の記録

#### 2026-10-27、yanma さんの Linux（QEMU 仮想マシン、PipeWire、仮想 HDA Intel）

libjack はこの環境に入っていないので、JACK のログ（#34）は確認していません。

| 項目 | ビルド | 結果 | 判定 |
| --- | --- | --- | --- |
| PipeWire の `default` で `output-info` | `cf935eb`（#31） | 終了 0。48000Hz / f32 / 2ch。`ALSA lib` の行は 0 | PASS |
| PipeWire の `default` で `probe-output 1` | `cf935eb` | 終了 0。48000 フレーム、不足 0。録音すると 440Hz の正弦波で、速さのずれと音割れはない | PASS |
| 未ログインで `tui` を起動し `q` で終了 | `cf935eb` | 終了 0。stderr は 0 バイト。画面に英語のログは出ない | PASS（再生なし） |
| 終了後の `pw-cli` | `cf935eb` | radiko-client のノードは残っていない | PASS |
| `.asoundrc` で既定を `hw:0,0` にして `output-info` | `cf935eb` | 48000Hz / i16 / 2ch | PASS |
| 同じ設定で `probe-output 1` | `cf935eb` | 終了 0。不足 0。録音すると 440Hz の正弦波 | PASS |
| 機器を 44100Hz だけに固定して `probe-output 1` | `cf935eb` | 終了 1。日本語のエラー1行（「必要な形式は48000Hz…機器の対応範囲: 44100Hz・i16・2ch」）だけ。`ALSA lib` は 0 行。別のレートでは開かない | PASS |
| `play-live FMJ` を90分（16:30:15〜18:00:16 JST） | #33 のビルド | 終了 0、「指定時間で停止しました」。259,200,000 フレーム（5400秒ちょうど）、1082 セグメント、不足 0。HTTP は最後まで 200、通信失敗 0、`ALSA lib` 0 行。取得に最大 5843ms かかった区間があるが、不足は出ていない | PASS |
| 再生中に終了 | #32 のビルド | すぐに閉じる（yanma さんが操作して確認） | PASS |

- 90分の再生中に、再認証は一度も起きていません（0回）。HTTP 401 のときに配信認証をやり直す処理は、プロセス内の疑似 Fetch を使う自動テストでしか確認していません。本物の HTTP サーバーが 401 を返す場合は #12 で扱います。
- 90分の確認は CLI の `play-live` です。TUI の「配信認証を更新して継続」の表示は確認していません。
- `hw:0,0` の確認で、サウンドサーバーを止めていたかは記録していません。

#### 2026-10-26〜27、QA のコンテナ（Debian 13、`/dev/snd` なし。実機ではない）

| 項目 | ビルド | 結果 |
| --- | --- | --- |
| PulseAudio 17.0 の `module-null-sink`（`libasound2-plugins` 経由の `default`）で `output-info` | `a5b8786` | 終了 0。48000Hz / f32 / 2ch。`ALSA lib` 0 行、stderr 0 行 |
| 同じ環境で `probe-output --mute 3` | `a5b8786` | 終了 0。144,000 フレーム、48000Hz、f32、2ch、不足 0。`ALSA lib` 0 行 |
| 同じ環境で ignored の `native_muted` 2件 | `1b6ce3b` | 2/2 成功 |
| 同じ環境で ignored の `native_muted` 2件 | `a5b8786` | 4回実行し、最初の1回だけ `native_muted_acquisition_gap_distinguishes_startup_from_filled_prefetch` が失敗。失敗の詳細は取っていません。そのあとの3回（並列2回、`--test-threads=1` が1回）は2件とも成功し、`gap_after=4` の不足は 0 |
| ignored の `local_mock_http` 2件 | `1b6ce3b`、`a5b8786` | どちらも 2/2 成功 |
| ALSA の `null` PCM | `1b6ce3b` | 実時間で進まないため、`native_muted` の不足の判定に使えない。製品の不具合ではない |
| 機器なし（サウンドサーバーも停止）で `output-info` と `probe-output 1` | `a5b8786` | どちらも終了 1。「エラー: 既定の音声出力機器がありません」の1行で、`ALSA lib` は 0 行。ただし libjack（`libjack-jackd2-0`）が入っているため、JACK のログが10行出る（#34） |

### 手動テストの記入表

実施者は yanma さん（日本、プレミアム会員）です。まだ行っていない項目は「未実施」としています。失敗も、そのまま書きます。

#### Q1. Ubuntu 24.04 + PipeWire、タイムフリー1800秒以上

| 項目 | 結果 |
| --- | --- |
| 日付・コミット | 未実施 |
| 環境（OS、カーネル、`pactl info` の Server Name、出力機器、物理 PC か仮想マシンか） | 未実施 |
| Q1-A 枠（局、日付、開始） | 未実施 |
| Q1-A `complete`、`output_seconds`、`final_underrun_frames`、`terminal_restored`、`exit_code` | 未実施 |
| Q1-A `underrun_observed` や `failed` の行 | 未実施 |
| Q1-B `grep -c "ALSA lib"` | 未実施 |
| Q1-B 画面の崩れ、聴こえ方、終了後の端末 | 未実施 |

参考: 未ログインの `tui` を起動して `q` で終了する確認は、上の表のとおり済んでいます（再生なし）。#26 の条件（不足 0、`ALSA lib` 0 行、端末の復元）は、Q1 の実施で判定します。

#### Q2. 出力確認と60秒ライブ

| 環境 | 日付・コミット | `output-info` | `probe-output 1` | `play-live <局> --seconds 60` | `ALSA lib` の行数 |
| --- | --- | --- | --- | --- | --- |
| Q2-W PipeWire | 2026-10-27 `cf935eb` | 終了 0、48000Hz / f32 / 2ch | 終了 0、不足 0 | 未実施（代わりに #33 のビルドで FMJ の90分、終了 0、不足 0） | 0 |
| Q2-P PulseAudio のみ | 未実施 | 未実施 | 未実施 | 未実施 | 未実施 |
| Q2-A 素の ALSA（`hw:0,0`） | 2026-10-27 `cf935eb` | 48000Hz / i16 / 2ch | 終了 0、不足 0 | 未実施 | 0（44100Hz 固定時のエラーも 0） |
| Q2-N 機器なし（物理 PC） | 未実施 | 未実施 | 未実施 | — | 未実施 |

Q2-N の参考として、QA のコンテナ（実機ではない）の結果は上の表にあります。

#### Q3. ライブ90分以上（#4 の修正の前と後）

| 項目 | 局・番組 | 完走 | 止まった時点 | 不足 | 備考 |
| --- | --- | --- | --- | --- | --- |
| 修正前 | 未実施（任意。止まる時期が決まっていないため、結果から分かることは少ない見込み） | — | — | — | — |
| 修正後（#33 のビルド、CLI） | FMJ、16:30:15〜18:00:16 JST | はい（5400秒、終了 0） | — | 0 | 再認証は0回で、再認証の経路は通っていない。TUI での90分は未実施 |

#### Q4. エリアの境界

| ケース | 日付・コミット | 終了コード | エラー文 |
| --- | --- | --- | --- |
| `session` の契約表示（エリアフリー、タイムフリー30） | 未実施 | — | — |
| Q4-1 地元局のライブ | 未実施 | 未実施 | 未実施 |
| Q4-2 地域外のライブ（エリアフリーあり） | 未実施 | 未実施 | 未実施 |
| Q4-3 地域外のタイムフリー（エリアフリーあり） | 未実施 | 未実施 | 未実施 |
| Q4-4 地域外（エリアフリーなしの会員） | 未実施 | 未実施 | 未実施 |
| Q4-5 番組表の3地域（ログイン不要） | 未実施 | 未実施 | 未実施 |
| Q4-6 TUI で地域外の番組を Enter | 未実施 | 未実施 | 未実施 |

#### Q5. USB DAC の抜き差しとスリープ復帰（タイムフリー）

`scripts/verify_tui_playback.py` は、Linux ではまだ実行していません。

| 項目 | 日付・コミット | 結果 |
| --- | --- | --- |
| Q5-1 DAC の機種と `output-info` | 未実施 | 未実施 |
| Q5-1 `--device-check`：抜いたときの `playback_error_observed`、`complete`、`final_underrun_frames`、`terminal_restored` | 未実施 | 未実施 |
| Q5-2 `--sleep-check`：`complete`、復帰後の通知、`terminal_restored` | 未実施 | 未実施 |

仮想マシンでのスリープと USB の受け渡しは、物理 PC と同じ動きとは限りません。できれば物理 PC で行い、仮想マシンで行ったときはそのことを記録します。
