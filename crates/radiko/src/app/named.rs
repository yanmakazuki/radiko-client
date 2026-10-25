//! Explicit local search-library operations, serialized off the UI thread.
use crate::{
    domain::search::SearchCriteria,
    storage::searches::{Error as StoreError, SearchStore},
};
use std::{collections::BTreeMap, fmt, path::PathBuf, sync::Arc};
use tokio::task::JoinHandle;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewId(pub u64);
pub enum Operation {
    List,
    Recall(String),
    Save(String, Box<SearchCriteria>),
    Delete(String),
}
impl Operation {
    fn mutating(&self) -> bool {
        matches!(self, Self::Save(..) | Self::Delete(..))
    }
}
pub enum Outcome {
    Listed(BTreeMap<String, SearchCriteria>),
    Recalled(Box<SearchCriteria>),
    Saved(String),
    Deleted(String),
}
#[derive(Debug)]
pub enum Error {
    Store(StoreError),
    Worker,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            Self::Worker => f.write_str("保存条件の操作が異常終了しました"),
        }
    }
}
pub struct Event {
    pub view: ViewId,
    pub mutating: bool,
    pub result: Result<Outcome, Error>,
}
struct Active {
    view: ViewId,
    mutating: bool,
    handle: JoinHandle<Result<Outcome, StoreError>>,
}
type Work = dyn Fn(Operation) -> Result<Outcome, StoreError> + Send + Sync;
pub struct Service {
    work: Arc<Work>,
    active: Option<Active>,
}
impl Service {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self::with_store(move |operation| {
            let store = match &directory {
                Some(path) => SearchStore::new(path.clone()),
                None => SearchStore::default_location()?,
            };
            execute(&store, operation)
        })
    }
    pub fn with_store(
        work: impl Fn(Operation) -> Result<Outcome, StoreError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            work: Arc::new(work),
            active: None,
        }
    }
    pub fn submit(&mut self, view: ViewId, operation: Operation) -> Result<(), &'static str> {
        if self.active.is_some() {
            return Err("保存条件の操作中です。完了後に再度操作してください");
        }
        let mutating = operation.mutating();
        let work = self.work.clone();
        let handle = tokio::task::spawn_blocking(move || work(operation));
        self.active = Some(Active {
            view,
            mutating,
            handle,
        });
        Ok(())
    }
    pub fn writing(&self) -> bool {
        self.active.as_ref().is_some_and(|active| active.mutating)
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
            .and_then(|r| r.map_err(Error::Store));
        Some(Event {
            view: active.view,
            mutating: active.mutating,
            result,
        })
    }
}
// Dropping a view/service does not abort an accepted atomic write. Runtime shutdown has its usual deadline.
fn execute(store: &SearchStore, operation: Operation) -> Result<Outcome, StoreError> {
    match operation {
        Operation::List => Ok(Outcome::Listed(store.load()?.named)),
        Operation::Recall(name) => Ok(Outcome::Recalled(Box::new(
            store
                .load()?
                .named
                .remove(&name)
                .ok_or(StoreError::NotFound)?,
        ))),
        Operation::Save(name, criteria) => {
            store.save_named(&name, *criteria)?;
            Ok(Outcome::Saved(name))
        }
        Operation::Delete(name) => {
            store.delete_named(&name)?;
            Ok(Outcome::Deleted(name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        BroadcastRange,
        search::{Filter, Scope},
    };
    use std::{
        fs,
        os::unix::{fs::PermissionsExt, io::AsRawFd},
    };
    fn criteria(day: u32) -> SearchCriteria {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, day).unwrap();
        SearchCriteria::new(
            Scope::Station("TBS".into()),
            BroadcastRange::new(day, day.succ_opt().unwrap()).unwrap(),
            Filter::default(),
        )
        .unwrap()
    }
    fn directory() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    #[test]
    fn named_save_recall_delete_preserve_saved_dates_and_last_conditions() {
        let dir = directory();
        let store = SearchStore::new(dir.path().join("data"));
        assert!(
            matches!(execute(&store,Operation::List).unwrap(),Outcome::Listed(items) if items.is_empty())
        );
        assert!(!store.path().exists());
        store.save_last(criteria(17)).unwrap();
        execute(
            &store,
            Operation::Save("昼番組".into(), Box::new(criteria(6))),
        )
        .unwrap();
        let Outcome::Recalled(recalled) =
            execute(&store, Operation::Recall("昼番組".into())).unwrap()
        else {
            panic!()
        };
        assert_eq!(*recalled, criteria(6));
        assert_eq!(store.load().unwrap().last, Some(criteria(17)));
        execute(&store, Operation::Delete("昼番組".into())).unwrap();
        assert!(store.load().unwrap().named.is_empty());
        assert_eq!(store.load().unwrap().last, Some(criteria(17)));
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn recalling_rereads_the_store_instead_of_using_the_browser_snapshot() {
        let dir = directory();
        let store = SearchStore::new(dir.path().to_owned());
        store.save_named("条件", criteria(6)).unwrap();
        let Outcome::Listed(items) = execute(&store, Operation::List).unwrap() else {
            panic!()
        };
        assert_eq!(items["条件"], criteria(6));
        store.save_named("条件", criteria(15)).unwrap();
        let Outcome::Recalled(recalled) =
            execute(&store, Operation::Recall("条件".into())).unwrap()
        else {
            panic!()
        };
        assert_eq!(*recalled, criteria(15));
        store.delete_named("条件").unwrap();
        assert!(matches!(
            execute(&store, Operation::Recall("条件".into())),
            Err(StoreError::NotFound)
        ));
    }
    #[test]
    fn corrupt_or_locked_storage_is_not_overwritten_by_mutations() {
        let dir = directory();
        let store = SearchStore::new(dir.path().to_owned());
        store.save_last(criteria(17)).unwrap();
        let original = fs::read(store.path()).unwrap();
        fs::write(store.path(), b"broken").unwrap();
        assert!(
            execute(
                &store,
                Operation::Save("条件".into(), Box::new(criteria(6)))
            )
            .is_err()
        );
        assert_eq!(fs::read(store.path()).unwrap(), b"broken");
        fs::write(store.path(), &original).unwrap();
        let lock = fs::File::open(dir.path().join("searches.lock")).unwrap();
        // SAFETY: the owned lock file keeps the descriptor valid until this test ends.
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(
            execute(
                &store,
                Operation::Save("条件".into(), Box::new(criteria(6)))
            )
            .is_err()
        );
        assert_eq!(fs::read(store.path()).unwrap(), original);
    }
    #[test]
    fn invalid_names_are_rejected_without_creating_a_store() {
        let dir = directory();
        let store = SearchStore::new(dir.path().join("data"));
        assert!(matches!(
            execute(
                &store,
                Operation::Save(" 不正 ".into(), Box::new(criteria(6)))
            ),
            Err(StoreError::InvalidName)
        ));
        assert!(!dir.path().join("data").exists());
    }
}
