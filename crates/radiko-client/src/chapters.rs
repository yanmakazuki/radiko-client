//! 公式チャプターの取得・一覧表示CLI。参考位置の表示に限定し、ジャンプ再生は行わない。
use crate::stdout::stdout_ln;
use chrono::FixedOffset;
use radiko::api::stream::{
    self,
    chapters::{self, Outcome},
};

const USAGE: &str = "chapters STATION YYYYMMDDHHmmss（日本時間、取得・一覧表示のみ）";
pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args == ["--help"] {
        stdout_ln!("{USAGE}");
        return Ok(());
    }
    let (station, cursor) = request(args)?;
    let report = chapters::inspect(station, cursor)?;
    let jst = FixedOffset::east_opt(9 * 3600).ok_or("日本時間を構成できません")?;
    stdout_ln!(
        "{} / {} / {}–{}（日本時間）",
        report.title.as_deref().unwrap_or("番組名未取得"),
        report.station,
        report
            .starts_at
            .with_timezone(&jst)
            .format("%Y-%m-%d %H:%M:%S"),
        report
            .ends_at
            .with_timezone(&jst)
            .format("%Y-%m-%d %H:%M:%S")
    );
    let aligned = matches!(&report.outcome, Outcome::Aligned(_, _));
    match report.outcome {
        Outcome::Unresolved(issue) => stdout_ln!("{}（取得・位置補正を省略）", issue.label()),
        Outcome::Unavailable => stdout_ln!("チャプター未提供（生成待ちの場合があります）"),
        Outcome::Candidates(set) | Outcome::Aligned(set, _) => {
            if aligned {
                stdout_ln!("放送枠・イベント原点一致（このCLIは閲覧のみ）");
            } else {
                stdout_ln!("メタデータ一致・音声位置未検証（閲覧のみ）");
            }
            stdout_ln!(
                "{}件 / 楽曲除外{}件 / 不正位置等{}件",
                set.chapters.len(),
                set.music_count,
                set.invalid_count
            );
            for (index, chapter) in set.chapters.iter().enumerate() {
                stdout_ln!(
                    "{}: {:.3}–{:.3}秒 {}",
                    index + 1,
                    chapter.start.as_secs_f64(),
                    chapter.end.as_secs_f64(),
                    chapter.title
                );
            }
        }
    }
    Ok(())
}
fn request(
    args: &[String],
) -> Result<(&str, chrono::DateTime<FixedOffset>), Box<dyn std::error::Error>> {
    if args.len() != 2 {
        return Err(USAGE.into());
    }
    stream::validate_station(&args[0])?;
    Ok((&args[0], stream::parse_time(&args[1])?))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_station_or_time_is_rejected_before_network() {
        for values in [
            vec![],
            vec!["TBS"],
            vec!["../TBS", "20261015120000"],
            vec!["TBS", "20260230000000"],
            vec!["TBS", "20261015120000", "extra"],
        ] {
            assert!(request(&values.iter().map(|s| s.to_string()).collect::<Vec<_>>()).is_err());
        }
        let args = ["TBS".into(), "20261015120000".into()];
        let (station, time) = request(&args).unwrap();
        assert_eq!(station, "TBS");
        assert_eq!(time.offset().local_minus_utc(), 9 * 3600);
    }
}
