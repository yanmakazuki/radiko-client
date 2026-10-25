//! 取得は一度に一つ実行し、待機要求は最新の一つだけ保持する。進捗通知は容量4に制限する。
use crate::{
    api::catalog::CatalogError,
    catalog::{self, Report, Source},
    domain::{
        Program,
        search::{Scope, SearchCriteria},
    },
};
use chrono::NaiveDate;
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{sync::mpsc, task::JoinHandle};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchId(pub u64);
pub struct Progress {
    pub phase: &'static str,
    pub target: String,
    pub day: Option<NaiveDate>,
    pub completed: usize,
    pub total: usize,
    pub failed: usize,
    pub programs: Vec<Program>,
}
#[derive(Debug)]
pub enum Error {
    Catalog(CatalogError),
    Worker,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(e) => e.fmt(f),
            Self::Worker => f.write_str("検索処理が異常終了しました"),
        }
    }
}
pub enum Event {
    Progress {
        id: SearchId,
        progress: Progress,
    },
    Finished {
        id: SearchId,
        result: Result<Report, Error>,
    },
}
impl Event {
    pub fn id(&self) -> SearchId {
        match self {
            Self::Progress { id, .. } | Self::Finished { id, .. } => *id,
        }
    }
}
struct Active {
    id: SearchId,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<Result<Report, CatalogError>>,
}
type Work = dyn Fn(
        SearchId,
        SearchCriteria,
        Arc<AtomicBool>,
        mpsc::Sender<Event>,
    ) -> Result<Report, CatalogError>
    + Send
    + Sync;
pub struct Service {
    work: Arc<Work>,
    active: Option<Active>,
    pending: Option<(SearchId, SearchCriteria)>,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
}
impl Service {
    pub fn new<S: Source + Send + 'static>(
        factory: impl Fn() -> Result<S, CatalogError> + Send + Sync + 'static,
    ) -> Self {
        Self::new_cancellable(move |_| factory())
    }
    pub fn new_cancellable<S: Source + Send + 'static>(
        factory: impl Fn(Arc<AtomicBool>) -> Result<S, CatalogError> + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel(4);
        let work = Arc::new(
            move |id,
                  criteria: SearchCriteria,
                  cancel: Arc<AtomicBool>,
                  tx: mpsc::Sender<Event>| {
                if cancel.load(Ordering::Acquire) {
                    return Err(CatalogError::Cancelled);
                }
                let source = factory(cancel.clone())?;
                let progress = |p: catalog::Progress<'_>| {
                    if cancel.load(Ordering::Acquire) {
                        return;
                    }
                    let progress = Progress {
                        phase: p.phase,
                        target: p.target.into(),
                        day: p.day,
                        completed: p.completed,
                        total: p.total,
                        failed: p.failed,
                        programs: p.list.map(|l| l.programs.clone()).unwrap_or_default(),
                    };
                    if tx.blocking_send(Event::Progress { id, progress }).is_err() {
                        cancel.store(true, Ordering::Release);
                    }
                };
                match criteria.scope() {
                    Scope::Station(station) => catalog::acquire_stations_controlled(
                        &source,
                        std::slice::from_ref(station),
                        criteria.range(),
                        &cancel,
                        progress,
                    ),
                    Scope::Areas(areas) => catalog::acquire_range_controlled(
                        &source,
                        areas,
                        criteria.range(),
                        &cancel,
                        progress,
                    ),
                    Scope::Nationwide => catalog::acquire_range_controlled(
                        &source,
                        &(1..=47).map(|n| format!("JP{n}")).collect::<Vec<_>>(),
                        criteria.range(),
                        &cancel,
                        progress,
                    ),
                }
            },
        );
        Self {
            work,
            active: None,
            pending: None,
            tx,
            rx,
        }
    }
    pub fn submit(&mut self, id: SearchId, criteria: SearchCriteria) {
        if let Some(active) = &self.active {
            active.cancel.store(true, Ordering::Release);
        }
        // 入力が続いても要求を積み上げず、次に実行する条件を上書きする。
        self.pending = Some((id, criteria));
        self.launch();
    }
    fn launch(&mut self) {
        // 実行中HTTPの終了を待ってから次世代を開始し、全体の並列上限を守る。
        if self.active.is_some() {
            return;
        }
        if let Some((id, criteria)) = self.pending.take() {
            let cancel = Arc::new(AtomicBool::new(false));
            let (work, stop, tx) = (self.work.clone(), cancel.clone(), self.tx.clone());
            let handle = tokio::task::spawn_blocking(move || work(id, criteria, stop, tx));
            self.active = Some(Active { id, cancel, handle });
        }
    }
    pub async fn poll(&mut self) -> Vec<Event> {
        let mut events = vec![];
        for _ in 0..4 {
            let Ok(event) = self.rx.try_recv() else { break };
            events.push(event);
        }
        if self.active.as_ref().is_some_and(|a| a.handle.is_finished()) {
            if let Some(active) = self.active.take() {
                let result = active
                    .handle
                    .await
                    .map_err(|_| Error::Worker)
                    .and_then(|r| r.map_err(Error::Catalog));
                // 最初の受信後に届いた最終進捗も、完了イベントより先に反映する。
                for _ in 0..4 {
                    let Ok(event) = self.rx.try_recv() else { break };
                    events.push(event);
                }
                events.push(Event::Finished {
                    id: active.id,
                    result,
                });
            }
            self.launch();
        }
        events
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        if let Some(active) = &self.active {
            active.cancel.store(true, Ordering::Release);
            active.handle.abort();
        }
    }
}
