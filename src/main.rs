use radiko_client::radiko::{MAX_CHAPTER_BYTES, parse_chapters};
use std::{fs::File, io::Read, process::ExitCode, time::Duration};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() || args == ["--help"] {
        println!("radiko-client: 初期検証用CLI（再生・ログイン未実装）\n\n  radiko-client inspect-chapters FILE SLOT_SECONDS VERIFIED_OFFSET_SECONDS\n\n合成fixtureなどのJSONを解析します。実番組の対応付けや聴取権限は確認しません。");
        return Ok(());
    }
    if args.len() != 4 || args[0] != "inspect-chapters" {
        return Err("引数が不正です。--helpを参照してください".into());
    }
    let seconds: f64 = args[2].parse().map_err(|_| "放送枠の秒数が不正です")?;
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| "放送枠の秒数が不正です")?;
    let offset: f64 = args[3].parse().map_err(|_| "補正秒数が不正です")?;
    let mut bytes = Vec::new();
    File::open(&args[1])?.take((MAX_CHAPTER_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    let set = parse_chapters(&bytes, duration, offset)?;
    if set.chapters.is_empty() { println!("チャプター未提供"); }
    for chapter in &set.chapters {
        println!("{:.3}–{:.3}秒  {}", chapter.start.as_secs_f64(), chapter.end.as_secs_f64(), chapter.title);
    }
    println!("不正項目: {} / 除外した楽曲: {}", set.invalid_count, set.music_count);
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => { eprintln!("エラー: {error}"); ExitCode::FAILURE }
    }
}
