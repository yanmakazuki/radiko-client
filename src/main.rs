use radiko_client::{
    radiko::{MAX_CHAPTER_BYTES, auth::MemberClient, parse_chapters},
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
        println!(
            "radiko-client: 検証用CLI（音声再生・TUI未実装）\n\n  radiko-client [--data-dir DIR] login\n  radiko-client [--data-dir DIR] session\n  radiko-client [--data-dir DIR] logout\n  radiko-client inspect-chapters FILE SLOT_SECONDS VERIFIED_OFFSET_SECONDS\n\nloginは端末でメールアドレスと非表示のパスワードを入力します。\nsessionは保存済みセッションの有効性と契約を確認します。\nlogoutはローカルの保存済みセッションを削除します。\ninspect-chaptersはローカルJSONを解析し、聴取権限は確認しません。"
        );
        return Ok(());
    }
    if args.len() == 1 && matches!(args[0].as_str(), "login" | "session" | "logout") {
        let store = match data_dir {
            Some(path) => SessionStore::new(path),
            None => SessionStore::default_location()?,
        };
        return member_command(&args[0], &store);
    }
    if args.len() != 4 || args[0] != "inspect-chapters" {
        return Err("引数が不正です。--helpを参照してください".into());
    }
    let seconds: f64 = args[2].parse().map_err(|_| "放送枠の秒数が不正です")?;
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| "放送枠の秒数が不正です")?;
    let offset: f64 = args[3].parse().map_err(|_| "補正秒数が不正です")?;
    let mut bytes = Vec::new();
    File::open(&args[1])?
        .take((MAX_CHAPTER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let set = parse_chapters(&bytes, duration, offset)?;
    if set.chapters.is_empty() {
        println!("チャプター未提供");
    }
    for chapter in &set.chapters {
        println!(
            "{:.3}–{:.3}秒  {}",
            chapter.start.as_secs_f64(),
            chapter.end.as_secs_f64(),
            chapter.title
        );
    }
    println!(
        "不正項目: {} / 除外した楽曲: {}",
        set.invalid_count, set.music_count
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
        println!("ローカルのセッションを削除しました");
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
            println!("未ログインです。loginでログインしてください");
            return Ok(());
        };
        let membership = MemberClient::new()?.check(&session)?;
        print_membership(&membership);
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
    println!("ログインし、セッションを保存しました");
    print_membership(&membership);
    Ok(())
}

fn print_membership(membership: &Membership) {
    fn label(value: Option<bool>) -> &'static str {
        match value {
            Some(true) => "あり",
            Some(false) => "なし",
            None => "未確認",
        }
    }
    println!(
        "ログイン有効 / エリアフリー: {} / タイムフリー30: {}",
        label(membership.area_free),
        label(membership.time_free_30)
    );
    println!("個別番組の再生可否は未確認です");
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("エラー: {error}");
            ExitCode::FAILURE
        }
    }
}
