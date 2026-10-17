//! メインループが所有するアプリ状態。検索の状態更新から再生コマンドは送らない。
pub mod search;
use crate::{
    catalog,
    domain::{Program, search::SearchCriteria},
    player::state::PlaybackSnapshot,
};
use search::{Event, SearchId};

#[derive(Debug, PartialEq, Eq)]
pub enum SearchStatus {
    Idle,
    Loading,
    Ready,
    Partial,
    Failed,
}
pub struct AppState {
    /// 検索に適用済みの条件。編集中の値とは独立して保持する。
    pub committed: SearchCriteria,
    pub draft: Option<SearchCriteria>,
    pub status: SearchStatus,
    pub progress: Option<search::Progress>,
    pub programs: Vec<Program>,
    /// 元の番組配列へのインデックス。描画のたびに再絞り込みしない。
    pub visible: Vec<usize>,
    pub area_failures: Vec<catalog::Failure>,
    pub schedule_failures: Vec<catalog::Failure>,
    pub invalid_count: usize,
    pub notification: String,
    pub playback: Option<PlaybackSnapshot>,
    id: SearchId,
}
impl AppState {
    pub fn new(criteria: SearchCriteria) -> Self {
        Self {
            committed: criteria,
            draft: None,
            status: SearchStatus::Idle,
            progress: None,
            programs: vec![],
            visible: vec![],
            area_failures: vec![],
            schedule_failures: vec![],
            invalid_count: 0,
            notification: String::new(),
            playback: None,
            id: SearchId(0),
        }
    }
    pub fn begin_edit(&mut self) {
        self.draft = Some(self.committed.clone());
    }
    pub fn edit_keyword(&mut self, keyword: Option<String>) -> Result<(), &'static str> {
        let draft = self
            .draft
            .as_ref()
            .ok_or("検索条件を編集中ではありません")?;
        self.draft = Some(SearchCriteria::new(
            draft.scope().clone(),
            draft.range().clone(),
            draft.filter().with_keyword(keyword)?,
        )?);
        Ok(())
    }
    pub fn cancel_edit(&mut self) {
        self.draft = None;
    }
    /// 条件を確定して世代を進める。実際の取得開始は呼び出し元が行う。
    pub fn commit(&mut self) -> Result<SearchId, &'static str> {
        let next = self.id.0.checked_add(1).ok_or("検索要求数の上限です")?;
        if let Some(draft) = self.draft.take() {
            self.committed = draft;
        }
        self.id = SearchId(next);
        self.status = SearchStatus::Loading;
        // 前回の一覧で未取得範囲を補完せず、今回の取得結果だけを表示する。
        self.programs.clear();
        self.visible.clear();
        self.area_failures.clear();
        self.schedule_failures.clear();
        self.invalid_count = 0;
        self.progress = None;
        self.notification.clear();
        Ok(self.id)
    }
    pub fn apply(&mut self, event: Event) {
        // キャンセル後に遅れて届いた旧検索の結果を、新しい一覧へ混ぜない。
        if event.id() != self.id {
            return;
        }
        match event {
            Event::Progress { mut progress, .. } => {
                for program in std::mem::take(&mut progress.programs) {
                    if self.committed.filter().matches(&program) {
                        self.visible.push(self.programs.len());
                    }
                    self.programs.push(program);
                }
                self.progress = Some(progress);
            }
            Event::Finished { result, .. } => match result {
                Ok(report) => {
                    self.status = if report.complete() {
                        SearchStatus::Ready
                    } else {
                        SearchStatus::Partial
                    };
                    self.invalid_count = report.lists.iter().map(|list| list.invalid_count).sum();
                    self.area_failures = report.area_failures;
                    self.schedule_failures = report.schedule_failures;
                    self.programs = report
                        .lists
                        .into_iter()
                        .flat_map(|list| list.programs)
                        .collect();
                    self.programs.sort_by(|a, b| {
                        (&a.key.starts_at, &a.key.station_id)
                            .cmp(&(&b.key.starts_at, &b.key.station_id))
                    });
                    self.visible = self
                        .programs
                        .iter()
                        .enumerate()
                        .filter_map(|(i, p)| self.committed.filter().matches(p).then_some(i))
                        .collect();
                }
                Err(error) => {
                    self.status = SearchStatus::Failed;
                    self.notification = error.to_string();
                }
            },
        }
    }
}
