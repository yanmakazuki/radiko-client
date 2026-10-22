//! 連続再生CLIの操作と通知。認証確認、割り込み、対話コマンドを再生処理へ接続する。
use chrono::{DateTime, FixedOffset, Utc};
use radiko_client::player::seek::{Command, Controller, PlaybackRequest};
use radiko_client::{
    player::{
        output::StopToken,
        playback::{self, EndReason},
    },
    radiko::{
        auth::MemberClient,
        stream::{
            self, ProbeMode,
            playback::{SegmentSource, prepare_at_controlled},
        },
    },
    storage::SessionStore,
};
use std::{
    io::{self, BufRead},
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};

const USAGE: &str = "play-live STATION [--interactive] [--seconds N] / play-timefree STATION YYYYMMDDHHmmss [--offset SECONDS] [--interactive] [--seconds N]（N: 1〜86400、未指定は番組末尾まで）";
struct Request {
    station: String,
    mode: ProbeMode,
    cursor: DateTime<FixedOffset>,
    seconds: Option<u32>,
    offset: Duration,
    interactive: bool,
}
fn request(
    args: &[String],
    now: DateTime<FixedOffset>,
) -> Result<Request, Box<dyn std::error::Error>> {
    let mut positional = Vec::new();
    let mut seconds = None;
    let mut offset = None;
    let mut interactive = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--seconds" && seconds.is_none() {
            let value = iter
                .next()
                .ok_or(USAGE)?
                .parse::<u32>()
                .map_err(|_| USAGE)?;
            if !(1..=86400).contains(&value) {
                return Err(USAGE.into());
            }
            seconds = Some(value);
        } else if arg == "--offset" && offset.is_none() {
            offset = Some(parse_position(iter.next().ok_or(USAGE)?)?);
        } else if arg == "--interactive" && !interactive {
            interactive = true;
        } else if arg.starts_with("--") {
            return Err(USAGE.into());
        } else {
            positional.push(arg);
        }
    }
    let mode = match args.first().map(String::as_str) {
        Some("play-live") if positional.len() == 1 => ProbeMode::Live,
        Some("play-timefree") if positional.len() == 2 => ProbeMode::TimeFree,
        _ => return Err(USAGE.into()),
    };
    if mode == ProbeMode::Live && offset.is_some() {
        return Err("--offsetはタイムフリーで使用してください".into());
    }
    stream::validate_station(positional[0])?;
    let cursor = if mode == ProbeMode::Live {
        now
    } else {
        stream::parse_time(positional[1])?
    };
    if mode == ProbeMode::TimeFree && cursor > now {
        return Err(stream::ProbeError::Upcoming.into());
    }
    Ok(Request {
        station: positional[0].clone(),
        mode,
        cursor,
        seconds,
        offset: offset.unwrap_or(Duration::ZERO),
        interactive,
    })
}

fn parse_position(value: &str) -> Result<Duration, &'static str> {
    let seconds = value.parse::<f64>().map_err(|_| "秒数を指定してください")?;
    if !seconds.is_finite() || !(0.0..=86400.0).contains(&seconds) {
        return Err("位置は0〜86400秒で指定してください");
    }
    Duration::try_from_secs_f64(seconds).map_err(|_| "秒数が不正です")
}
fn command(line: &str) -> Result<Command, &'static str> {
    match line.trim() {
        value if value.starts_with("live ") || value.starts_with("timefree ") => {
            let mut args: Vec<String> = value.split_whitespace().map(str::to_string).collect();
            args[0] = format!("play-{}", args[0]);
            let target = request(&args, Utc::now().fixed_offset())
                .map_err(|_| "live 局ID / timefree 局ID YYYYMMDDHHmmss [--offset 秒数] を指定してください")?;
            if target.seconds.is_some() || target.interactive { return Err("切替先に--secondsと--interactiveは指定できません"); }
            Ok(Command::Select(PlaybackRequest { station: target.station, mode: target.mode,
                cursor: target.cursor, position: target.offset }))
        }
        "status" => Ok(Command::Status),
        "pause" => Ok(Command::Pause),
        "resume" => Ok(Command::Resume),
        value if value.starts_with("volume ") => {
            let volume = value[7..]
                .trim()
                .parse::<u32>()
                .map_err(|_| "音量は0〜100で指定してください")?;
            if volume > 100 {
                return Err("音量は0〜100で指定してください");
            }
            Ok(Command::Volume(volume))
        }
        "+15" => Ok(Command::Relative(15)),
        "-15" => Ok(Command::Relative(-15)),
        "q" | "stop" => Ok(Command::Stop),
        value => value
            .strip_prefix("seek ")
            .ok_or("live 局ID / timefree 局ID YYYYMMDDHHmmss [--offset 秒数] / status / pause / resume / volume 0〜100 / seek 秒数 / +15 / -15 / q を入力してください")
            .and_then(parse_position)
            .map(Command::Seek),
    }
}
fn display_time(time: DateTime<FixedOffset>) -> String {
    FixedOffset::east_opt(9 * 3600)
        .map(|jst| time.with_timezone(&jst))
        .unwrap_or(time)
        .to_rfc3339()
}
fn describe(p: &playback::Progress) -> String {
    use radiko_client::player::state::PlaybackState;
    let state = match p.state {
        PlaybackState::Buffering => "再生準備中",
        PlaybackState::Playing => "再生中",
        PlaybackState::Paused => "一時停止中",
    };
    let position = p
        .position
        .map(|position| format!("再生位置: {:.3}秒", position.as_secs_f64()))
        .unwrap_or_else(|| "ライブ（番組内位置未確認）".into());
    let output = p.output.as_ref().map_or_else(
        || "出力待ち".into(),
        |output| {
            format!(
                "出力 {:.1}秒 / バッファ不足: {}フレーム",
                output.frames as f64 / f64::from(output.sample_rate.max(1)),
                output.underrun_frames
            )
        },
    );
    let pending = p.pending.as_ref().map_or_else(String::new, |target| {
        format!(
            " / 切替準備中: {} / {:?} / {} / {:.3}秒",
            target.station,
            target.mode,
            display_time(target.cursor),
            target.position.as_secs_f64()
        )
    });
    format!(
        "{} / {} / {} / {:?} / {}–{} / {} / 音量{}% / {} / {}セグメント{}\n{}",
        state,
        p.info.title.as_deref().unwrap_or("番組名未取得"),
        p.info.station,
        p.info.mode,
        display_time(p.info.starts_at),
        display_time(p.info.ends_at),
        position,
        p.volume,
        output,
        p.segments,
        pending,
        p.pipeline
            .as_ref()
            .map(|p| p.lines())
            .unwrap_or_else(|| "処理診断 未取得".into())
    )
}
fn check_preparation(stop: &StopToken) -> Result<(), playback::PlaybackError> {
    if stop.is_cancelled() {
        Err(playback::PlaybackError::Source(
            radiko_client::radiko::stream::ProbeError::Cancelled,
        ))
    } else {
        Ok(())
    }
}
pub fn run(args: &[String], data_dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() == 2 && args[1] == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    let request = request(args, Utc::now().fixed_offset())?;
    let store = match data_dir {
        Some(path) => SessionStore::new(path),
        None => SessionStore::default_location()?,
    };
    let session = Arc::new(
        store
            .load()?
            .ok_or("未ログインです。loginでログインしてください")?,
    );
    let stop = StopToken::default();
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.cancel())
        .map_err(|_| "終了シグナルの停止処理を登録できません")?;
    let auth_stop = stop.clone();
    let membership = MemberClient::new_cancellable(Arc::new(move || auth_stop.is_cancelled()))
        .and_then(|client| client.check(&session));
    if stop.is_cancelled() {
        println!("中断しました");
        return Ok(());
    }
    let membership = membership?;
    eprintln!("放送枠・配信条件を確認して連続再生を準備しています…");
    let initial_stop = stop.clone();
    let prepared = prepare_at_controlled(
        &request.station,
        request.mode,
        request.cursor,
        &session,
        &membership,
        request.offset,
        Arc::new(move || initial_stop.is_cancelled()),
    );
    if stop.is_cancelled() {
        println!("中断しました");
        return Ok(());
    }
    let source = prepared?;
    let info = source.info();
    let jst = FixedOffset::east_opt(9 * 3600).ok_or("日本時間を構成できません")?;
    println!(
        "{} / {} / {:?} / {}–{}（日本時間） / Ctrl+Cで停止",
        info.title.as_deref().unwrap_or("番組名未取得"),
        info.station,
        info.mode,
        info.starts_at
            .with_timezone(&jst)
            .format("%Y-%m-%d %H:%M:%S"),
        info.ends_at.with_timezone(&jst).format("%Y-%m-%d %H:%M:%S")
    );
    let progress = |p: playback::Progress| {
        eprintln!("{}", describe(&p));
    };
    let report = if request.interactive {
        let (tx, rx) = mpsc::sync_channel(4);
        let input_stop = stop.clone();
        std::thread::spawn(move || {
            for line in io::stdin().lock().lines() {
                if input_stop.is_cancelled() {
                    break;
                }
                let Ok(line) = line else {
                    break;
                };
                match command(&line) {
                    Ok(command) => {
                        if tx.send(command).is_err() {
                            break;
                        }
                    }
                    Err(error) => eprintln!("{error}"),
                }
            }
        });
        let mut controls = Controller::new_cancellable(rx, stop.clone(), move |target, cancel| {
            check_preparation(&cancel)?;
            let auth_cancel = cancel.clone();
            let membership =
                MemberClient::new_cancellable(Arc::new(move || auth_cancel.is_cancelled()))
                    .and_then(|c| c.check(&session))
                    .map_err(playback::PlaybackError::Auth)?;
            check_preparation(&cancel)?;
            prepare_at_controlled(
                &target.station,
                target.mode,
                target.cursor,
                &session,
                &membership,
                target.position,
                Arc::new(move || cancel.is_cancelled()),
            )
            .map(|s| Box::new(s) as Box<dyn SegmentSource>)
            .map_err(playback::PlaybackError::Source)
        });
        eprintln!(
            "操作: live 局ID / timefree 局ID YYYYMMDDHHmmss [--offset 秒数] / status / pause / resume / volume 0〜100 / seek 秒数 / +15 / -15 / q（Enterで確定）"
        );
        playback::play_controlled(source, request.seconds, stop, &mut controls, progress)?
    } else {
        playback::play(source, request.seconds, stop, progress)?
    };
    println!(
        "{}",
        match report.reason {
            EndReason::ProgramEnd => "番組終了",
            EndReason::Limit => "指定時間で停止しました",
            EndReason::Cancelled => "中断しました",
        }
    );
    if let Some(output) = report.output {
        println!(
            "出力: {}フレーム / {}Hz / {}ch / {}セグメント / バッファ不足: {}フレーム",
            output.frames,
            output.sample_rate,
            output.channels,
            report.segments,
            output.underrun_frames
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_formats_paused_title_jst_and_pending_target_without_claiming_live_position() {
        use radiko_client::{
            player::state::{PlaybackSnapshot, PlaybackState},
            radiko::stream::playback::StreamInfo,
        };
        assert!(matches!(command("status"), Ok(Command::Status)));
        let start = DateTime::parse_from_rfc3339("2026-10-15T03:00:00Z").unwrap();
        let p = PlaybackSnapshot {
            pipeline: None,
            info: StreamInfo {
                title: Some("現在の番組".into()),
                station: "TBS".into(),
                starts_at: start,
                ends_at: start + chrono::Duration::hours(1),
                mode: ProbeMode::Live,
                position: Duration::ZERO,
            },
            state: PlaybackState::Paused,
            pending: Some(PlaybackRequest {
                station: "OBS".into(),
                mode: ProbeMode::Live,
                cursor: start,
                position: Duration::ZERO,
            }),
            volume: 10,
            position: None,
            output: None,
            segments: 0,
        };
        let text = describe(&p);
        for expected in [
            "一時停止中",
            "現在の番組",
            "TBS",
            "2026-10-15T12:00:00+09:00",
            "音量10%",
            "切替準備中: OBS",
            "番組内位置未確認",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("再生位置:"));
    }
    #[test]
    fn parses_station_switch_and_rejects_invalid_or_future_targets() {
        let Command::Select(live) = command("live OBS").unwrap() else {
            panic!()
        };
        assert_eq!(live.station, "OBS");
        assert_eq!(live.mode, ProbeMode::Live);
        let Command::Select(past) = command("timefree TBS 20261015120000 --offset 60.5").unwrap()
        else {
            panic!()
        };
        assert_eq!(past.position, Duration::from_millis(60_500));
        assert_eq!(past.cursor, stream::parse_time("20261015120000").unwrap());
        for input in [
            "live ../OBS",
            "live OBS --offset 0",
            "live OBS --seconds 30",
            "live OBS --interactive",
            "timefree TBS 20990101000000",
            "timefree TBS 20260230000000",
            "timefree TBS 20261015120000 --offset NaN",
            "live OBS extra",
        ] {
            assert!(command(input).is_err(), "{input}");
        }
    }
    #[test]
    fn parses_pause_resume_and_bounded_volume() {
        assert!(matches!(command("pause"), Ok(Command::Pause)));
        assert!(matches!(command("resume"), Ok(Command::Resume)));
        assert!(matches!(command("volume 0"), Ok(Command::Volume(0))));
        assert!(matches!(command("volume 100"), Ok(Command::Volume(100))));
        for value in [
            "volume -1",
            "volume 101",
            "volume NaN",
            "volume",
            "volume 1.5",
        ] {
            assert!(command(value).is_err());
        }
    }
    #[test]
    fn seek_commands_and_offsets_reject_invalid_numbers_before_network() {
        for text in ["seek NaN", "seek inf", "seek -1", "seek 86401", "arbitrary"] {
            assert!(command(text).is_err());
        }
        assert!(
            matches!(command("seek 60.5").unwrap(),Command::Seek(d) if d==Duration::from_millis(60_500))
        );
        assert!(matches!(command("+15").unwrap(), Command::Relative(15)));
        assert!(matches!(command("-15").unwrap(), Command::Relative(-15)));
        assert!(matches!(command("q").unwrap(), Command::Stop));
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let now = stream::parse_time("20261016120000").unwrap();
        assert!(request(&args(&["play-live", "TBS", "--offset", "0"]), now).is_err());
        assert!(
            request(
                &args(&["play-timefree", "TBS", "20261015120000", "--offset", "NaN"]),
                now
            )
            .is_err()
        );
        let r = request(
            &args(&[
                "play-timefree",
                "TBS",
                "20261015120000",
                "--offset",
                "60.5",
                "--interactive",
            ]),
            now,
        )
        .unwrap();
        assert_eq!(r.offset, Duration::from_millis(60_500));
        assert!(r.interactive);
    }
    #[test]
    fn validates_duration_station_and_future_time_before_network() {
        let args = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        let now = stream::parse_time("20261016120000").unwrap();
        for values in [
            vec!["play-live"],
            vec!["play-live", "../TBS"],
            vec!["play-live", "TBS", "--seconds", "0"],
            vec!["play-live", "TBS", "--seconds", "86401"],
            vec!["play-live", "TBS", "--seconds", "1", "--seconds", "2"],
            vec!["play-timefree", "TBS", "20990101000000"],
            vec!["play-timefree", "TBS", "20260230000000"],
        ] {
            assert!(request(&args(&values), now).is_err());
        }
        let live = request(&args(&["play-live", "OBS", "--seconds", "30"]), now).unwrap();
        assert_eq!(live.seconds, Some(30));
        assert_eq!(live.cursor, now);
        let archive = request(&args(&["play-timefree", "TBS", "20261006120000"]), now).unwrap();
        assert_eq!(archive.mode, ProbeMode::TimeFree);
        assert_eq!(archive.seconds, None);
    }
}
