use super::*;
use ratatui::{Terminal, backend::TestBackend};

mod chapters;
mod compact;
mod device;
mod editing;
mod field;
mod keys;
mod notices;
mod on_air;
mod output;
mod progress;
mod status;
mod truncate;
mod wording;

fn screen() -> Screen {
    let now = radiko::api::stream::parse_time("20261015060000").unwrap();
    let criteria = crate::programs::criteria(&["TBS".into(), "20261015".into()], now).unwrap();
    Screen::new(criteria, String::new())
}
