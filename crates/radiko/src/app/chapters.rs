//! Independent chapter acquisition. Only slot-bound aligned timelines may request seeking.
use crate::{
    api::stream::chapters::{self, BindingIssue, EventBinding, Outcome, Report},
    domain::{ChapterSet, Program, ProgramKey},
};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId(u64);
#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    NotRequested,
    Loading,
    Ready,
    Unavailable,
    Unresolved(BindingIssue),
    Failed(String),
}
pub struct State {
    pub target: Option<Program>,
    pub status: Status,
    pub candidates: Option<ChapterSet>,
    binding: Option<EventBinding>,
    id: RequestId,
}
impl Default for State {
    fn default() -> Self {
        Self {
            target: None,
            status: Status::NotRequested,
            candidates: None,
            binding: None,
            id: RequestId(0),
        }
    }
}
impl State {
    pub fn open(&mut self, target: Program) -> Result<RequestId, &'static str> {
        self.id = RequestId(
            self.id
                .0
                .checked_add(1)
                .ok_or("チャプター要求番号の上限です")?,
        );
        self.target = Some(target);
        self.status = Status::Loading;
        self.candidates = None;
        self.binding = None;
        Ok(self.id)
    }
    pub fn close(&mut self) {
        self.target = None;
        self.status = Status::NotRequested;
        self.candidates = None;
        self.binding = None;
    }
    pub fn apply(&mut self, event: Event) {
        let Some(target) = &self.target else { return };
        if event.id != self.id || event.key != target.key {
            return;
        }
        self.candidates = None;
        self.binding = None;
        match event.result {
            Ok(report) => {
                if report.station != target.key.station_id
                    || report.starts_at != target.key.starts_at
                    || report.ends_at != target.ends_at
                {
                    self.status = Status::Unresolved(BindingIssue::SlotMismatch);
                    return;
                }
                self.status = match report.outcome {
                    Outcome::Aligned(set, binding) => {
                        if !binding.matches(&target.key, target.ends_at) {
                            self.status = Status::Unresolved(BindingIssue::SlotMismatch);
                            return;
                        }
                        self.binding = Some(binding);
                        self.candidates = Some(set);
                        Status::Ready
                    }
                    Outcome::Candidates(set) => {
                        self.candidates = Some(set);
                        Status::Ready
                    }
                    Outcome::Unavailable => Status::Unavailable,
                    Outcome::Unresolved(issue) => Status::Unresolved(issue),
                };
            }
            Err(error) => self.status = Status::Failed(error.to_string()),
        }
    }
    pub fn label(&self) -> &str {
        match &self.status {
            Status::NotRequested => "未取得",
            Status::Loading => "チャプター取得中",
            Status::Ready if self.binding.is_some() => {
                "放送枠・イベント原点一致（ジャンプ可能・聴取可否は再確認）"
            }
            Status::Ready => "メタデータ一致・音声位置未検証（閲覧のみ）",
            Status::Unavailable => "チャプター未提供（生成待ちの場合があります）",
            Status::Unresolved(issue) => issue.label(),
            Status::Failed(_) => "チャプター取得失敗",
        }
    }
    pub fn jump(&self, index: usize) -> Result<(&Program, std::time::Duration), &'static str> {
        let target = self.target.as_ref().ok_or("対象番組がありません")?;
        if self.status != Status::Ready
            || !self
                .binding
                .as_ref()
                .is_some_and(|b| b.matches(&target.key, target.ends_at))
        {
            return Err(self.jump_refusal());
        }
        let chapter = self
            .candidates
            .as_ref()
            .and_then(|set| set.chapters.get(index))
            .ok_or("チャプターを選択してください")?;
        let length = (target.ends_at - target.key.starts_at)
            .to_std()
            .map_err(|_| "放送枠の長さが不正です")?;
        let start = chapter.start;
        let end = chapter.end;
        if start >= end || end > length {
            return Err("放送枠外のチャプターにはジャンプできません");
        }
        Ok((target, start))
    }
    pub fn jump_refusal(&self) -> &'static str {
        match self.status {
            Status::Ready => "音声との時刻対応が未検証のためジャンプできません（現在の再生を継続）",
            _ => "ジャンプできるチャプターはありません（現在の再生を継続）",
        }
    }
}
#[derive(Debug)]
pub enum Error {
    Acquire(chapters::Error),
    Worker,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acquire(error) => error.fmt(f),
            Self::Worker => f.write_str("チャプター取得処理が異常終了しました"),
        }
    }
}
pub struct Event {
    pub id: RequestId,
    pub key: ProgramKey,
    pub result: Result<Report, Error>,
}
struct Request {
    id: RequestId,
    target: Program,
}
struct Active {
    id: RequestId,
    key: ProgramKey,
    cancelled: Arc<AtomicBool>,
    handle: JoinHandle<Result<Report, chapters::Error>>,
}
type Work = dyn Fn(&Program, Arc<AtomicBool>) -> Result<Report, chapters::Error> + Send + Sync;
pub struct Service {
    work: Arc<Work>,
    active: Option<Active>,
    pending: Option<Request>,
}
impl Default for Service {
    fn default() -> Self {
        Self::with_cancellable_source(|program, cancelled| {
            chapters::inspect_cancellable(&program.key.station_id, program.key.starts_at, cancelled)
        })
    }
}
impl Service {
    pub fn with_source(
        work: impl Fn(&Program, &AtomicBool) -> Result<Report, chapters::Error> + Send + Sync + 'static,
    ) -> Self {
        Self::with_cancellable_source(move |program, cancel| work(program, &cancel))
    }
    pub fn with_cancellable_source(
        work: impl Fn(&Program, Arc<AtomicBool>) -> Result<Report, chapters::Error>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            work: Arc::new(work),
            active: None,
            pending: None,
        }
    }
    pub fn submit(&mut self, id: RequestId, target: Program) {
        self.cancel();
        self.pending = Some(Request { id, target });
        self.launch();
    }
    pub fn cancel(&mut self) {
        self.pending = None;
        if let Some(active) = &self.active {
            active.cancelled.store(true, Ordering::Release);
        }
    }
    fn launch(&mut self) {
        if self.active.is_some() {
            return;
        }
        if let Some(request) = self.pending.take() {
            let cancelled = Arc::new(AtomicBool::new(false));
            let (work, stop) = (self.work.clone(), cancelled.clone());
            let key = request.target.key.clone();
            let handle = tokio::task::spawn_blocking(move || {
                if stop.load(Ordering::Acquire) {
                    return Err(chapters::Error::Cancelled);
                }
                work(&request.target, stop)
            });
            self.active = Some(Active {
                id: request.id,
                key,
                cancelled,
                handle,
            });
        }
    }
    pub async fn poll(&mut self) -> Option<Event> {
        if !self.active.as_ref().is_some_and(|a| a.handle.is_finished()) {
            return None;
        }
        let active = self.active.take()?;
        let result = active
            .handle
            .await
            .map_err(|_| Error::Worker)
            .and_then(|r| r.map_err(Error::Acquire));
        self.launch();
        Some(Event {
            id: active.id,
            key: active.key,
            result,
        })
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.cancel();
        if let Some(active) = &self.active {
            active.handle.abort();
        }
    }
}
