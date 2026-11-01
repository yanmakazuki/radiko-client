//! Saved-session checks and explicit local logout; results never contain credentials.
use crate::{
    api::auth::{AuthError, MemberClient},
    session::{MemberSession, Membership},
    storage::{SessionStore, StorageError},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::task::JoinHandle;

#[derive(Debug, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    NotChecked,
    Checking,
    LoggingOut,
    LoggingIn,
    LoginFailed(AuthError),
    LoginSaveFailed(StorageError),
    LoginWorkerFailed,
    LogoutFailed(StorageError),
    LogoutWorkerFailed,
    SignedOut,
    Verified(Membership),
    Expired,
    StorageFailed(StorageError),
    CheckFailed(AuthError),
    WorkerFailed,
}
impl Status {
    pub fn summary(&self) -> &'static str {
        match self {
            Self::NotChecked => "セッション未確認",
            Self::Checking => "セッション確認中",
            Self::LoggingOut => "ログアウト処理中",
            Self::LoggingIn => "ログイン処理中",
            Self::LoginFailed(_) => "ログイン失敗",
            Self::LoginSaveFailed(_) => "ログイン後のセッション保存失敗",
            Self::LoginWorkerFailed => "ログイン結果未確認",
            Self::LogoutWorkerFailed => "ログアウト失敗（削除未確認）",
            Self::LogoutFailed(_) => "ログアウト失敗（削除未確認）",
            Self::SignedOut => "未ログイン",
            Self::Verified(_) => "ログイン確認済み",
            Self::Expired => "未ログイン（セッション失効）",
            Self::StorageFailed(_) => "未ログイン（保存セッション読み込み失敗）",
            Self::CheckFailed(_) => "セッション確認失敗（有効性未確認）",
            Self::WorkerFailed => "セッション確認失敗",
        }
    }
    pub fn detail(&self) -> String {
        match self {
            Self::NotChecked => "a 保存セッションを確認".into(),
            Self::Checking => "番組探索と現在の再生を継続できます".into(),
            Self::LoggingOut => "再生を停止して保存セッションを削除しています".into(),
            Self::LoggingIn => "認証とセッション保存を行っています。現在の再生は継続します".into(),
            Self::LoginFailed(error) => format!("{error} / a で再確認 / I で再ログイン"),
            Self::LoginSaveFailed(error) => {
                format!("{error} / 保存結果を確認してください / I 再ログイン")
            }
            Self::LoginWorkerFailed => {
                "認証処理が異常終了しました。aで保存状態を確認してください".into()
            }
            Self::LogoutWorkerFailed => {
                "ログアウト処理が異常終了しました / O 再試行 / a 保存状態を確認".into()
            }
            Self::LogoutFailed(error) => format!("{error} / O 再試行 / a 保存状態を確認"),
            Self::SignedOut => "a で再確認 / I でログインしてください".into(),
            Self::Verified(membership) => format!(
                "エリアフリー: {} / 30日: {} / 再生可否は番組ごとに確認 / a 再確認 / O ログアウト",
                flag(membership.area_free),
                flag(membership.time_free_30)
            ),
            Self::Expired => "a で再確認 / I で再ログインしてください".into(),
            Self::StorageFailed(error) => format!("{error} / loginで再ログイン / a 再確認"),
            Self::CheckFailed(error) => format!("{error} / a 再確認"),
            Self::WorkerFailed => "認証確認処理が異常終了しました / a 再確認".into(),
        }
    }
}
fn flag(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "有効",
        Some(false) => "未加入",
        None => "未確認",
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId(u64);
#[derive(Default)]
pub struct State {
    pub status: Status,
    generation: u64,
}
impl State {
    pub fn begin(&mut self) -> Result<RequestId, &'static str> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("認証確認番号の上限です")?;
        self.status = Status::Checking;
        Ok(RequestId(self.generation))
    }
    pub fn begin_login(&mut self) -> Result<RequestId, &'static str> {
        let id = self.begin()?;
        self.status = Status::LoggingIn;
        Ok(id)
    }
    pub fn begin_logout(&mut self) -> Result<RequestId, &'static str> {
        let id = self.begin()?;
        self.status = Status::LoggingOut;
        Ok(id)
    }
    pub fn apply(&mut self, event: Event) {
        if event.id.0 == self.generation {
            self.status = event.status;
        }
    }
}
pub struct Event {
    pub id: RequestId,
    pub status: Status,
}
struct Active {
    id: RequestId,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<Status>,
}
type Work = dyn Fn(Arc<AtomicBool>) -> Status + Send + Sync;
pub struct Service {
    work: Arc<Work>,
    active: Option<Active>,
    pending: Option<RequestId>,
}
impl Service {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self::with_cancellable_check(move |cancel| {
            let store = match &data_dir {
                Some(path) => Ok(SessionStore::new(path.clone())),
                None => SessionStore::default_location(),
            };
            match store {
                Ok(store) => check_saved(&store, &cancel, |session| {
                    let flag = cancel.clone();
                    MemberClient::new_cancellable(Arc::new(move || flag.load(Ordering::Acquire)))
                        .and_then(|client| client.check(session))
                }),
                Err(error) => Status::StorageFailed(error),
            }
        })
    }
    pub fn with_check(work: impl Fn(&AtomicBool) -> Status + Send + Sync + 'static) -> Self {
        Self::with_cancellable_check(move |cancel| work(&cancel))
    }
    pub fn with_cancellable_check(
        work: impl Fn(Arc<AtomicBool>) -> Status + Send + Sync + 'static,
    ) -> Self {
        Self {
            work: Arc::new(work),
            active: None,
            pending: None,
        }
    }
    pub fn cancel(&mut self) {
        self.pending = None;
        if let Some(active) = &self.active {
            active.cancel.store(true, Ordering::Release);
        }
    }
    pub fn submit(&mut self, id: RequestId) {
        if let Some(active) = &self.active {
            active.cancel.store(true, Ordering::Release);
        }
        self.pending = Some(id);
        self.launch();
    }
    fn launch(&mut self) {
        if self.active.is_some() {
            return;
        }
        if let Some(id) = self.pending.take() {
            let cancel = Arc::new(AtomicBool::new(false));
            let (work, stop) = (self.work.clone(), cancel.clone());
            let handle = tokio::task::spawn_blocking(move || {
                if stop.load(Ordering::Acquire) {
                    Status::NotChecked
                } else {
                    work(stop)
                }
            });
            self.active = Some(Active { id, cancel, handle });
        }
    }
    pub async fn poll(&mut self) -> Option<Event> {
        if !self.active.as_ref().is_some_and(|a| a.handle.is_finished()) {
            return None;
        }
        let active = self.active.take()?;
        let status = active.handle.await.unwrap_or(Status::WorkerFailed);
        self.launch();
        Some(Event {
            id: active.id,
            status,
        })
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

/// No Debug implementation: plaintext credentials never leave the submitted job.
pub struct Credentials {
    mail: zeroize::Zeroizing<String>,
    password: zeroize::Zeroizing<String>,
}
impl Credentials {
    pub fn new(
        mail: zeroize::Zeroizing<String>,
        password: zeroize::Zeroizing<String>,
    ) -> Result<Self, &'static str> {
        if mail.trim().is_empty()
            || password.is_empty()
            || mail.len() > 1024
            || password.len() > 4096
            || mail.chars().any(char::is_control)
            || password.chars().any(char::is_control)
        {
            return Err("メールアドレスとパスワードを確認してください");
        }
        Ok(Self { mail, password })
    }
}
pub struct LoginService {
    work: Arc<dyn Fn(Credentials) -> Status + Send + Sync>,
    active: Option<(RequestId, JoinHandle<Status>)>,
}
impl LoginService {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self::with_login(move |credentials| {
            let store = match &data_dir {
                Some(path) => Ok(SessionStore::new(path.clone())),
                None => SessionStore::default_location(),
            };
            let store = match store {
                Ok(store) => store,
                Err(error) => return Status::LoginSaveFailed(error),
            };
            login_saved(&store, credentials, |mail, password| {
                MemberClient::new().and_then(|client| client.login(mail, password))
            })
        })
    }
    pub fn with_login(work: impl Fn(Credentials) -> Status + Send + Sync + 'static) -> Self {
        Self {
            work: Arc::new(work),
            active: None,
        }
    }
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn submit(&mut self, id: RequestId, credentials: Credentials) -> Result<(), &'static str> {
        if self.busy() {
            return Err("ログイン処理中です");
        }
        let work = self.work.clone();
        self.active = Some((id, tokio::task::spawn_blocking(move || work(credentials))));
        Ok(())
    }
    pub async fn poll(&mut self) -> Option<Event> {
        if !self
            .active
            .as_ref()
            .is_some_and(|(_, task)| task.is_finished())
        {
            return None;
        }
        let (id, task) = self.active.take()?;
        Some(Event {
            id,
            status: task.await.unwrap_or(Status::LoginWorkerFailed),
        })
    }
}

fn login_saved(
    store: &SessionStore,
    credentials: Credentials,
    login: impl FnOnce(&str, &str) -> Result<(MemberSession, Membership), AuthError>,
) -> Status {
    let result = login(credentials.mail.trim(), &credentials.password);
    drop(credentials);
    let (session, membership) = match result {
        Ok(result) => result,
        Err(error) => return Status::LoginFailed(error),
    };
    match store.save(session) {
        Ok(()) => Status::Verified(membership),
        Err(error) => Status::LoginSaveFailed(error),
    }
}

/// Accepted deletion is never aborted when the confirmation UI closes.
pub struct LogoutService {
    delete: Arc<dyn Fn() -> Result<(), StorageError> + Send + Sync>,
    active: Option<(RequestId, JoinHandle<Result<(), StorageError>>)>,
}
impl LogoutService {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self::with_delete(move || {
            let store = match &data_dir {
                Some(path) => SessionStore::new(path.clone()),
                None => SessionStore::default_location()?,
            };
            store.delete()
        })
    }
    pub fn with_delete(
        delete: impl Fn() -> Result<(), StorageError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            delete: Arc::new(delete),
            active: None,
        }
    }
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn submit(&mut self, id: RequestId) -> Result<(), &'static str> {
        if self.busy() {
            return Err("ログアウト処理中です");
        }
        let delete = self.delete.clone();
        self.active = Some((id, tokio::task::spawn_blocking(move || delete())));
        Ok(())
    }
    pub async fn poll(&mut self) -> Option<Event> {
        if !self
            .active
            .as_ref()
            .is_some_and(|(_, task)| task.is_finished())
        {
            return None;
        }
        let (id, task) = self.active.take()?;
        let status = match task.await {
            Ok(Ok(())) => Status::SignedOut,
            Ok(Err(error)) => Status::LogoutFailed(error),
            Err(_) => Status::LogoutWorkerFailed,
        };
        Some(Event { id, status })
    }
}

fn check_saved(
    store: &SessionStore,
    cancelled: &AtomicBool,
    check: impl FnOnce(&MemberSession) -> Result<Membership, AuthError>,
) -> Status {
    if cancelled.load(Ordering::Acquire) {
        return Status::NotChecked;
    }
    let session = match store.load() {
        Ok(Some(session)) => session,
        Ok(None) => return Status::SignedOut,
        Err(error) => return Status::StorageFailed(error),
    };
    if cancelled.load(Ordering::Acquire) {
        return Status::NotChecked;
    }
    match check(&session) {
        Ok(membership) => Status::Verified(membership),
        Err(AuthError::SessionExpired) => Status::Expired,
        Err(error) => Status::CheckFailed(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    fn directory() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    fn credentials() -> Credentials {
        Credentials::new(
            zeroize::Zeroizing::new("user@example.invalid".into()),
            zeroize::Zeroizing::new("SYNTHETIC_PASSWORD_99".into()),
        )
        .unwrap()
    }
    #[test]
    fn failed_login_preserves_corruption_and_success_saves_only_a_session() {
        let dir = directory();
        let store = SessionStore::new(dir.path().to_owned());
        fs::write(store.path(), b"preserved corruption").unwrap();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
        let failed = login_saved(&store, credentials(), |_, _| {
            Err(AuthError::InvalidCredentials)
        });
        assert!(matches!(
            failed,
            Status::LoginFailed(AuthError::InvalidCredentials)
        ));
        assert_eq!(fs::read(store.path()).unwrap(), b"preserved corruption");
        let success = login_saved(&store, credentials(), |mail, password| {
            assert_eq!(mail, "user@example.invalid");
            assert_eq!(password, "SYNTHETIC_PASSWORD_99");
            Ok((
                MemberSession::new("SYNTHETIC_SESSION_ONLY".into()).unwrap(),
                Membership {
                    area_free: Some(true),
                    time_free_30: Some(false),
                },
            ))
        });
        assert!(matches!(success, Status::Verified(_)));
        let saved = fs::read_to_string(store.path()).unwrap();
        assert!(saved.contains("SYNTHETIC_SESSION_ONLY"));
        assert!(!saved.contains("PASSWORD"));
        assert!(!saved.contains("example.invalid"));
        assert_eq!(
            fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn login_save_failure_is_distinct_from_authentication_success() {
        let dir = directory();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let store = SessionStore::new(dir.path().to_owned());
        let status = login_saved(&store, credentials(), |_, _| {
            Ok((
                MemberSession::new("SYNTHETIC_SESSION".into()).unwrap(),
                Membership {
                    area_free: None,
                    time_free_30: None,
                },
            ))
        });
        assert!(matches!(
            status,
            Status::LoginSaveFailed(StorageError::UnsafePermissions)
        ));
        assert!(!store.path().exists());
        assert!(!status.detail().contains("SYNTHETIC"));
    }
    #[test]
    fn login_supersedes_old_checks_and_rejects_duplicate_jobs_without_queueing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut state = State::default();
            let old = state.begin().unwrap();
            let id = state.begin_login().unwrap();
            state.apply(Event {
                id: old,
                status: Status::SignedOut,
            });
            assert_eq!(state.status, Status::LoggingIn);
            let (entered, seen) = std::sync::mpsc::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let gate = std::sync::Mutex::new(gate);
            let mut service = LoginService::with_login(move |_| {
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                Status::LoginFailed(AuthError::InvalidCredentials)
            });
            service.submit(id, credentials()).unwrap();
            seen.recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert!(service.submit(id, credentials()).is_err());
            assert!(service.poll().await.is_none());
            release.send(()).unwrap();
            let began = std::time::Instant::now();
            let event = loop {
                if let Some(event) = service.poll().await {
                    break event;
                }
                assert!(began.elapsed() < std::time::Duration::from_secs(2));
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            };
            state.apply(event);
            assert!(matches!(
                state.status,
                Status::LoginFailed(AuthError::InvalidCredentials)
            ));
            assert!(!service.busy());
            assert!(seen.try_recv().is_err());
        });
    }
    #[test]
    fn accepted_login_keeps_atomic_save_when_the_service_view_is_dropped() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = directory();
            let directory = dir.path().to_owned();
            let (entered, seen) = std::sync::mpsc::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let (done, finished) = std::sync::mpsc::channel();
            let gate = std::sync::Mutex::new(gate);
            let mut service = LoginService::with_login(move |credentials| {
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                let status = login_saved(
                    &SessionStore::new(directory.clone()),
                    credentials,
                    |_, _| {
                        Ok((
                            MemberSession::new("SYNTHETIC_SESSION".into()).unwrap(),
                            Membership {
                                area_free: None,
                                time_free_30: None,
                            },
                        ))
                    },
                );
                done.send(()).unwrap();
                status
            });
            let mut state = State::default();
            service
                .submit(state.begin_login().unwrap(), credentials())
                .unwrap();
            seen.recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            drop(service);
            release.send(()).unwrap();
            finished
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert!(
                SessionStore::new(dir.path().to_owned())
                    .load()
                    .unwrap()
                    .is_some()
            );
        });
    }
    #[test]
    fn logout_invalidates_an_in_flight_verified_response() {
        let mut state = State::default();
        let check = state.begin().unwrap();
        let logout = state.begin_logout().unwrap();
        state.apply(Event {
            id: check,
            status: Status::Verified(Membership {
                area_free: Some(true),
                time_free_30: Some(true),
            }),
        });
        assert_eq!(state.status, Status::LoggingOut);
        state.apply(Event {
            id: logout,
            status: Status::SignedOut,
        });
        state.apply(Event {
            id: check,
            status: Status::Expired,
        });
        assert_eq!(state.status, Status::SignedOut);
    }
    #[test]
    fn explicit_logout_deletes_only_the_selected_store_and_missing_is_successful() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = directory();
            let session = dir.path().join("session.json");
            fs::write(&session, b"synthetic corrupt session").unwrap();
            fs::set_permissions(&session, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(dir.path().join("searches.json"), b"preserved").unwrap();
            let mut service = LogoutService::new(Some(dir.path().to_owned()));
            let mut state = State::default();
            for _ in 0..2 {
                service.submit(state.begin_logout().unwrap()).unwrap();
                let began = std::time::Instant::now();
                let event = loop {
                    if let Some(event) = service.poll().await {
                        break event;
                    }
                    assert!(began.elapsed() < std::time::Duration::from_secs(2));
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                };
                state.apply(event);
                assert_eq!(state.status, Status::SignedOut);
                assert!(!session.exists());
                assert_eq!(
                    fs::read(dir.path().join("searches.json")).unwrap(),
                    b"preserved"
                );
            }
        });
    }
    #[test]
    fn accepted_logout_is_not_queued_or_aborted_when_service_is_dropped() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (entered, seen) = std::sync::mpsc::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let (done, finished) = std::sync::mpsc::channel();
            let gate = std::sync::Mutex::new(gate);
            let mut service = LogoutService::with_delete(move || {
                entered.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                done.send(()).unwrap();
                Ok(())
            });
            let mut state = State::default();
            service.submit(state.begin_logout().unwrap()).unwrap();
            seen.recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert!(service.submit(state.begin_logout().unwrap()).is_err());
            assert!(service.poll().await.is_none());
            drop(service);
            release.send(()).unwrap();
            finished
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert!(seen.try_recv().is_err());
        });
    }
    #[test]
    fn logout_failure_does_not_claim_signed_out_or_remove_the_file() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = directory();
            let path = dir.path().join("session.json");
            fs::write(&path, b"preserved").unwrap();
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let mut service = LogoutService::new(Some(dir.path().to_owned()));
            let mut state = State::default();
            service.submit(state.begin_logout().unwrap()).unwrap();
            let began = std::time::Instant::now();
            let event = loop {
                if let Some(event) = service.poll().await {
                    break event;
                }
                assert!(began.elapsed() < std::time::Duration::from_secs(2));
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            };
            state.apply(event);
            assert!(matches!(
                state.status,
                Status::LogoutFailed(StorageError::UnsafePermissions)
            ));
            assert_eq!(fs::read(path).unwrap(), b"preserved");
        });
    }
    #[test]
    fn missing_corrupt_unsupported_and_unsafe_files_never_contact_server_or_modify_files() {
        let dir = directory();
        let store = SessionStore::new(dir.path().to_owned());
        let flag = AtomicBool::new(false);
        assert_eq!(
            check_saved(&store, &flag, |_| panic!("missing session must not check")),
            Status::SignedOut
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        for (bytes, permissions, expected) in [
            (&b"broken"[..], 0o600, StorageError::Invalid),
            (
                &br#"{"version":2,"session":{"token":"SYNTHETIC_SESSION"}}"#[..],
                0o600,
                StorageError::UnsupportedVersion,
            ),
            (&b"broken"[..], 0o644, StorageError::UnsafePermissions),
        ] {
            fs::write(store.path(), bytes).unwrap();
            fs::set_permissions(store.path(), fs::Permissions::from_mode(permissions)).unwrap();
            assert_eq!(
                check_saved(&store, &flag, |_| panic!("bad file must not check")),
                Status::StorageFailed(expected)
            );
            assert_eq!(fs::read(store.path()).unwrap(), bytes);
            assert_eq!(
                fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
                permissions
            );
        }
    }
    #[test]
    fn verified_contract_flags_expiry_and_network_uncertainty_preserve_session_and_hide_credentials()
     {
        let dir = directory();
        let store = SessionStore::new(dir.path().to_owned());
        const CANARY: &str = "SYNTHETIC_SESSION_CANARY";
        store
            .save(MemberSession::new(CANARY.into()).unwrap())
            .unwrap();
        let bytes = fs::read(store.path()).unwrap();
        let flag = AtomicBool::new(false);
        for (area_free, time_free_30) in [(Some(true), Some(false)), (None, Some(true))] {
            let state = check_saved(&store, &flag, |_| {
                Ok(Membership {
                    area_free,
                    time_free_30,
                })
            });
            assert!(matches!(state, Status::Verified(_)));
            let text = format!("{state:?} {} {}", state.summary(), state.detail());
            assert!(!text.contains(CANARY));
            assert!(state.detail().contains("再生可否は番組ごと"));
        }
        let expired = check_saved(&store, &flag, |_| Err(AuthError::SessionExpired));
        assert_eq!(expired, Status::Expired);
        assert_eq!(expired.detail(), "a で再確認 / I で再ログインしてください");
        assert_eq!(
            Status::SignedOut.detail(),
            "a で再確認 / I でログインしてください"
        );
        let unknown = check_saved(&store, &flag, |_| Err(AuthError::Network));
        assert_eq!(unknown, Status::CheckFailed(AuthError::Network));
        assert!(unknown.summary().contains("有効性未確認"));
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
    }
    #[test]
    fn cancelled_check_does_not_read_a_session_or_call_checker() {
        let dir = directory();
        let store = SessionStore::new(dir.path().to_owned());
        assert_eq!(
            check_saved(&store, &AtomicBool::new(true), |_| panic!("cancelled")),
            Status::NotChecked
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[test]
    fn old_verification_never_replaces_a_new_check_or_a_new_signed_out_result() {
        let mut state = State::default();
        let first = state.begin().unwrap();
        let latest = state.begin().unwrap();
        state.apply(Event {
            id: first,
            status: Status::Verified(Membership {
                area_free: Some(true),
                time_free_30: Some(true),
            }),
        });
        assert_eq!(state.status, Status::Checking);
        state.apply(Event {
            id: latest,
            status: Status::SignedOut,
        });
        state.apply(Event {
            id: first,
            status: Status::Expired,
        });
        assert_eq!(state.status, Status::SignedOut);
    }
}
