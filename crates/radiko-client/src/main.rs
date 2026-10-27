//! CLIの入口。サブコマンドへ処理を振り分け、結果や安全なエラー情報を表示する。
mod chapters;
mod play;
mod programs;
mod searches;
mod stdout;
mod tui;

use stdout::stdout_ln;

use radiko::{
    api::{
        MAX_CHAPTER_BYTES,
        auth::MemberClient,
        parse_chapters,
        stream::{self, ProbeMode},
    },
    player::{DecodeProbe, output},
    session::Membership,
    storage::SessionStore,
};
use std::{
    fs::File,
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (args, data_dir) = arguments(std::env::args().skip(1).collect())?;
    if args.is_empty() || args == ["--help"] {
        stdout_ln!(
            "radiko-client: 検証用CLI・検索／ライブ・タイムフリー再生TUI\n\n  radiko-client [--data-dir DIR] login\n  radiko-client [--data-dir DIR] session\n  radiko-client [--data-dir DIR] logout\n  radiko-client [--data-dir DIR] probe-live STATION\n  radiko-client [--data-dir DIR] probe-timefree STATION YYYYMMDDHHmmss\n  radiko-client [--data-dir DIR] play-live STATION [--interactive] [--seconds N]\n  radiko-client [--data-dir DIR] play-timefree STATION YYYYMMDDHHmmss [--offset SECONDS] [--interactive] [--seconds N]\n  radiko-client output-info [SAMPLE_RATE]\n  radiko-client probe-output [--mute] [SECONDS]\n  radiko-client [--data-dir DIR] play-sample STATION [YYYYMMDDHHmmss]\n  radiko-client programs STATION [START [END]] [--json]\n  radiko-client programs (--areas JP13,JP14 | --all) [START [END]] [--json]\n  radiko-client programs --help（絞り込みオプション）\n  radiko-client tui [programsと同じ検索条件]\n  radiko-client searches --help（条件の保存・一覧・削除）\n  radiko-client programs --saved NAME / --restore\n  radiko-client chapters STATION YYYYMMDDHHmmss\n  radiko-client inspect-chapters FILE SLOT_SECONDS VERIFIED_OFFSET_SECONDS\n\nloginは端末でメールアドレスと非表示のパスワードを入力します。\nsessionは保存済みセッションの有効性と契約を確認します。\nlogoutはローカルの保存済みセッションを削除します。\ninspect-chaptersはローカルJSONを解析し、聴取権限は確認しません。"
        );
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "tui") {
        return tui::run(&args[1..], data_dir);
    }
    if args.first().is_some_and(|arg| arg == "searches") {
        return searches::run(&args[1..], data_dir);
    }
    if args.first().is_some_and(|arg| arg == "chapters") {
        return chapters::run(&args[1..]);
    }
    if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "play-live" | "play-timefree"))
    {
        return play::run(&args, data_dir);
    }
    if args.first().is_some_and(|arg| arg == "output-info") {
        return output_info(&args);
    }
    if args.first().is_some_and(|arg| arg == "probe-output") {
        return probe_output(&args);
    }
    if args.first().is_some_and(|arg| arg == "programs") {
        return programs::run(&args[1..], data_dir);
    }
    if args.len() == 1 && matches!(args[0].as_str(), "login" | "session" | "logout") {
        let store = session_store(data_dir)?;
        return member_command(&args[0], &store);
    }
    if (args.len() == 2 && args[0] == "probe-live")
        || (args.len() == 3 && args[0] == "probe-timefree")
        || ((2..=3).contains(&args.len()) && args[0] == "play-sample")
    {
        return probe_media(&args, data_dir);
    }
    if args.len() == 4 && args[0] == "inspect-chapters" {
        return inspect_chapters(&args);
    }
    Err("引数が不正です。--helpを参照してください".into())
}

fn inspect_chapters(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let seconds: f64 = args[2].parse().map_err(|_| "放送枠の秒数が不正です")?;
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| "放送枠の秒数が不正です")?;
    let offset: f64 = args[3].parse().map_err(|_| "補正秒数が不正です")?;
    let mut bytes = Vec::new();
    File::open(&args[1])?
        .take((MAX_CHAPTER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let set = parse_chapters(&bytes, duration, offset)?;
    if set.chapters.is_empty() {
        stdout_ln!("チャプター未提供");
    }
    for chapter in &set.chapters {
        stdout_ln!(
            "{:.3}–{:.3}秒  {}",
            chapter.start.as_secs_f64(),
            chapter.end.as_secs_f64(),
            chapter.title
        );
    }
    stdout_ln!(
        "不正項目: {} / 除外した楽曲: {}",
        set.invalid_count,
        set.music_count
    );
    Ok(())
}

fn session_store(data_dir: Option<PathBuf>) -> Result<SessionStore, Box<dyn std::error::Error>> {
    Ok(match data_dir {
        Some(path) => SessionStore::new(path),
        None => SessionStore::default_location()?,
    })
}

fn probe_media(
    args: &[String],
    data_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    stream::validate_station(&args[1])?;
    let mode = if args[0] == "probe-live" || (args[0] == "play-sample" && args.len() == 2) {
        ProbeMode::Live
    } else {
        ProbeMode::TimeFree
    };
    let cursor = if mode == ProbeMode::Live {
        chrono::Utc::now().fixed_offset()
    } else {
        stream::parse_time(&args[2])?
    };
    if mode == ProbeMode::TimeFree && cursor > chrono::Utc::now() {
        return Err(stream::ProbeError::Upcoming.into());
    }
    let store = session_store(data_dir)?;
    let session = store
        .load()?
        .ok_or("未ログインです。loginでログインしてください")?;
    let membership = MemberClient::new()?.check(&session)?;
    eprintln!("会員セッション有効。放送枠・配信認証・HLSを検証しています…");
    let report = if args[0] == "play-sample" {
        stream::probe_for_output(&args[1], mode, cursor, &session, &membership)?
    } else {
        stream::probe(&args[1], mode, cursor, &session, &membership)?
    };
    let jst = chrono::FixedOffset::east_opt(9 * 3600).ok_or("日本時間を構成できません")?;
    stdout_ln!(
        "{} / {:?} / {}–{}（日本時間）",
        report.station,
        report.mode,
        report
            .starts_at
            .with_timezone(&jst)
            .format("%Y-%m-%d %H:%M:%S"),
        report
            .ends_at
            .with_timezone(&jst)
            .format("%Y-%m-%d %H:%M:%S")
    );
    stdout_ln!(
        "認証エリア: {} / エリア外: {} / 失敗した配信先: {}",
        report.area,
        report.outside_area,
        report.failed_endpoints
    );
    stdout_ln!(
        "HLS取得成功: {}セグメント / {:.3}秒 / ENDLIST: {}",
        report.segments,
        report.playlist_seconds,
        report.end_list
    );
    stdout_ln!(
        "HLS宣言コーデック: {} / サンプル形式: {} / {}バイト",
        report.declared_codec.as_deref().unwrap_or("未宣言"),
        report.container,
        report.sample_bytes
    );
    print_decode(report.decode)?;
    if args[0] == "play-sample" {
        let pcm = report
            .audio
            .as_ref()
            .ok_or("取得したサンプルは音声出力に対応していません")?;
        let remaining = if mode == ProbeMode::Live {
            Some(
                (report.ends_at - chrono::Utc::now().fixed_offset())
                    .to_std()
                    .map_err(|_| "番組終了")?,
            )
        } else {
            None
        };
        // The sample starts at the selected slot's beginning. Never output past its end.
        if mode == ProbeMode::TimeFree
            && pcm.summary.media_duration > (report.ends_at - report.starts_at).to_std()?
        {
            return Err("サンプルが放送枠の末尾を越えています".into());
        }
        let summary = output::play_pcm(pcm, remaining)?;
        stdout_ln!(
            "先頭1セグメントの出力完了: {}フレーム / {}Hz / {}ch / バッファ不足: {}フレーム",
            summary.frames,
            summary.sample_rate,
            summary.channels,
            summary.underrun_frames
        );
        stdout_ln!(
            "サンプル末尾で停止しました。連続再生にはplay-live／play-timefreeを使用してください"
        );
    } else {
        stdout_ln!("音声出力は行っていません。30分連続再生・実際の番組末尾の停止は未検証です");
    }
    Ok(())
}

fn print_decode(decode: DecodeProbe) -> Result<(), Box<dyn std::error::Error>> {
    match decode {
        DecodeProbe::AacDecoded(summary) => {
            stdout_ln!(
                "AACセグメントの純Rustデコード成功: {}パケット / {}フレーム / {}Hz / {}ch",
                summary.packets,
                summary.frames,
                summary.sample_rate,
                summary.channels
            );
            stdout_ln!(
                "AACコア: {}Hz / {}ch / SBR検出: {} / モノラルからステレオへの拡張: {}",
                summary.core_sample_rate,
                summary.core_channels,
                summary.sbr_detected,
                summary.stereo_expansion
            );
            stdout_ln!(
                "デコードしたメディア: {:.3}秒 / 処理時間: {:.3}秒 / 非ゼロサンプル: {}",
                summary.media_duration.as_secs_f64(),
                summary.decode_duration.as_secs_f64(),
                summary.nonzero_samples
            );
            stdout_ln!(
                "ADTSの公開デコードAPIにビットストリームのPSフラグはなく、明示シグナルは未確認です"
            );
        }
        DecodeProbe::RawAacDecoded(summary) => {
            stdout_ln!(
                "設定情報付きAACの先頭パケットを純Rustでデコード成功: {}Hz / {}ch / {}フレーム / SBR検出: {}",
                summary.sample_rate,
                summary.channels,
                summary.frames,
                summary.sbr_detected
            );
            stdout_ln!(
                "ASCのPS明示: {} / 先頭パケットの検証です。ビットストリーム上のPS適用とは区別します",
                if summary.explicit_ps {
                    "あり"
                } else {
                    "なし"
                }
            );
        }
        DecodeProbe::AacFailed(error) => stdout_ln!("サンプル取得済み。AAC検証失敗: {error}"),
        DecodeProbe::Decoded {
            codec,
            sample_rate,
            channels,
            frames,
        } => stdout_ln!(
            "先頭パケットの純Rustデコード成功: {codec} / {sample_rate}Hz / {channels}ch / {frames}フレーム"
        ),
        DecodeProbe::UnsupportedFormat => {
            stdout_ln!("サンプル取得済み。採用中のSymphoniaでコンテナを解析できません")
        }
        DecodeProbe::UnsupportedDeclaredCodec => stdout_ln!(
            "HE-AAC宣言に対応するコンテナまたは設定情報を確認できません（AACコアだけの成功とは扱いません）"
        ),
        DecodeProbe::UnsupportedCodec => {
            stdout_ln!("サンプル取得済み。採用中のSymphoniaでコーデックが未対応です")
        }
        DecodeProbe::DecodeFailed => stdout_ln!("サンプル取得済み。音声デコードに失敗しました"),
        DecodeProbe::NoAudio => {
            stdout_ln!("サンプル取得済み。有効な音声フレームを確認できません")
        }
        DecodeProbe::Encrypted => stdout_ln!("暗号化サンプル取得済み。復号・デコードは未実装です"),
    }
    Ok(())
}

fn output_info(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    const USAGE: &str =
        "output-info [SAMPLE_RATE]（8000〜192000Hz・既定48000Hz、出力機器の読み取りのみ）";
    if args == ["output-info", "--help"] {
        stdout_ln!("{USAGE}");
        return Ok(());
    }
    if args.len() > 2 {
        return Err(USAGE.into());
    }
    let rate = args
        .get(1)
        .map_or(Ok(48_000), |value| value.parse::<u32>())
        .map_err(|_| "サンプルレートは正の整数Hzで指定してください")?;
    if !output::PCM_SAMPLE_RATE_RANGE.contains(&rate) {
        return Err("サンプルレートは8000〜192000Hzで指定してください".into());
    }
    let info = output::inspect_default_output(rate)?;
    stdout_ln!("現在の既定出力機器: {}", info.name);
    match (info.sample_format, info.channels) {
        (Some(format), Some(channels)) => stdout_ln!(
            "{}Hz / {format} / {channels}ch: 対応宣言あり（再生時に再確認）",
            info.sample_rate
        ),
        _ => stdout_ln!("{}", info.mismatch),
    }
    Ok(())
}

fn probe_output(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.get(1).is_some_and(|arg| arg == "--help") && args.len() == 2 {
        stdout_ln!(
            "probe-output [SECONDS]（生成音・1〜10秒・既定2秒）\nprobe-output --mute [SECONDS]（無音・1〜1800秒・既定30秒、Ctrl+Cで中断）"
        );
        return Ok(());
    }
    let muted = args.get(1).is_some_and(|arg| arg == "--mute");
    let values = &args[if muted { 2 } else { 1 }..];
    if values.len() > 1 {
        return Err("probe-output [--mute] [SECONDS]（--helpで範囲を確認）".into());
    }
    let seconds = values
        .first()
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(if muted { 30 } else { 2 });
    if muted {
        if !(1..=1800).contains(&seconds) {
            return Err("無音の出力診断は1〜1800秒で指定してください".into());
        }
        let stop = output::StopToken::default();
        let cancelled = stop.clone();
        ctrlc::set_handler(move || cancelled.cancel())?;
        let summary = output::probe_silent(seconds, stop.clone(), |summary| {
            eprintln!(
                "無音出力中: {}フレーム / {}Hz / {} / 不足{}フレーム",
                summary.frames, summary.sample_rate, summary.sample_format, summary.underrun_frames
            );
        })?;
        stdout_ln!(
            "無音出力{}: {}フレーム / {}Hz / {} / {}ch / バッファ不足: {}フレーム",
            if stop.is_cancelled() {
                "中断"
            } else {
                "完了"
            },
            summary.frames,
            summary.sample_rate,
            summary.sample_format,
            summary.channels,
            summary.underrun_frames
        );
        return Ok(());
    }
    let summary = output::probe_tone(seconds)?;
    stdout_ln!(
        "生成音の出力完了: {}フレーム / {}Hz / {} / {}ch / バッファ不足: {}フレーム",
        summary.frames,
        summary.sample_rate,
        summary.sample_format,
        summary.channels,
        summary.underrun_frames
    );
    Ok(())
}

fn arguments(mut args: Vec<String>) -> Result<(Vec<String>, Option<PathBuf>), &'static str> {
    let mut data_dir = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--data-dir" {
            if data_dir.is_some()
                || index + 1 >= args.len()
                || args[index + 1].is_empty()
                || args[index + 1].starts_with("--")
            {
                return Err("--data-dirには保存先を1回だけ指定してください");
            }
            data_dir = Some(PathBuf::from(args.remove(index + 1)));
            args.remove(index);
        } else {
            index += 1;
        }
    }
    Ok((args, data_dir))
}

fn member_command(command: &str, store: &SessionStore) -> Result<(), Box<dyn std::error::Error>> {
    if command == "logout" {
        store.delete()?;
        stdout_ln!("ローカルのセッションを削除しました");
        return Ok(());
    }
    let saved = match store.load() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}。既存ファイルを保持して未ログインとして扱います");
            None
        }
    };
    if command == "session" {
        let Some(session) = saved else {
            stdout_ln!("未ログインです。loginでログインしてください");
            return Ok(());
        };
        let membership = MemberClient::new()?.check(&session)?;
        print_membership(&membership)?;
        return Ok(());
    }
    // Passwords are never accepted through argv, environment variables or pipes.
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err("loginは対話端末で実行してください".into());
    }
    eprint!("メールアドレス: ");
    io::stderr().flush()?;
    let mut mail = String::new();
    io::stdin().read_line(&mut mail)?;
    let password = zeroize::Zeroizing::new(rpassword::prompt_password("パスワード（非表示）: ")?);
    let result = MemberClient::new()?.login(mail.trim(), &password);
    drop(password);
    let (session, membership) = result?;
    store.save(session)?;
    stdout_ln!("ログインし、セッションを保存しました");
    print_membership(&membership)?;
    Ok(())
}

fn print_membership(membership: &Membership) -> Result<(), Box<dyn std::error::Error>> {
    fn label(value: Option<bool>) -> &'static str {
        match value {
            Some(true) => "あり",
            Some(false) => "なし",
            None => "未確認",
        }
    }
    stdout_ln!(
        "ログイン有効 / エリアフリー: {} / タイムフリー30: {}",
        label(membership.area_free),
        label(membership.time_free_30)
    );
    stdout_ln!("個別番組の再生可否は未確認です");
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if stdout::is_closed(error.as_ref()) => {
            ExitCode::from(stdout::BROKEN_PIPE_STATUS)
        }
        Err(error) => {
            eprintln!("エラー: {error}");
            ExitCode::FAILURE
        }
    }
}
