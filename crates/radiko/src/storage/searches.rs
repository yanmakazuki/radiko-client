//! バージョン付き検索条件の保存。番組表・分類一覧・認証情報・再生位置は含めない。
use super::*;
use crate::domain::{
    BroadcastRange,
    search::{Filter, Scope, SearchCriteria},
};
use chrono::NaiveDate;
use std::collections::BTreeMap;
const MAX_BYTES: u64 = 128 * 1024;
const MAX_NAMES: usize = 64;

#[derive(Debug)]
pub enum Error {
    Storage(StorageError),
    InvalidName,
    NotFound,
    TooMany,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => {
                f.write_str("条件名は前後空白・制御文字なしの1〜64文字で指定してください")
            }
            Self::NotFound => f.write_str("指定した名前の検索条件がありません"),
            Self::TooMany => f.write_str("名前付き検索条件は最大64件です"),
            Self::Storage(error) => match error {
                StorageError::Io(io::ErrorKind::WouldBlock) => {
                    f.write_str("別の処理が検索条件を更新中です。再実行してください")
                }
                StorageError::Io(kind) => {
                    write!(f, "検索条件ファイルの操作に失敗しました ({kind:?})")
                }
                StorageError::Invalid => {
                    f.write_str("検索条件ファイルの形式・値・サイズが不正です")
                }
                StorageError::UnsupportedVersion => f.write_str("未対応の検索条件保存形式です"),
                StorageError::UnsafePermissions => {
                    f.write_str("検索条件のディレクトリは0700、ファイルは0600にしてください")
                }
                StorageError::UnsafePath => {
                    f.write_str("検索条件保存先にリンクや通常ファイル以外は使用できません")
                }
                StorageError::NoDataDirectory => {
                    f.write_str("検索条件保存先を解決できません。--data-dirを指定してください")
                }
            },
        }
    }
}
impl std::error::Error for Error {}
impl From<StorageError> for Error {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Storage(value.into())
    }
}
fn invalid() -> Error {
    StorageError::Invalid.into()
}
fn check_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.trim() != name
        || name.chars().count() > 64
        || name.len() > 256
        || name.chars().any(char::is_control)
    {
        Err(Error::InvalidName)
    } else {
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriteriaDto {
    scope: Scope,
    start: String,
    end: String,
    filter: Filter,
}
impl CriteriaDto {
    fn from_criteria(value: &SearchCriteria) -> Self {
        Self {
            scope: value.scope().clone(),
            start: value.range().start().format("%Y%m%d").to_string(),
            end: value.range().end().format("%Y%m%d").to_string(),
            filter: value.filter().clone(),
        }
    }
    fn into_criteria(self) -> Result<SearchCriteria, Error> {
        let day = |text: &str| {
            if text.len() != 8 || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            NaiveDate::parse_from_str(text, "%Y%m%d").map_err(|_| invalid())
        };
        let range =
            BroadcastRange::new(day(&self.start)?, day(&self.end)?).map_err(|_| invalid())?;
        SearchCriteria::new(self.scope, range, self.filter).map_err(|_| invalid())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    last: Option<CriteriaDto>,
    named: BTreeMap<String, CriteriaDto>,
}
#[derive(Default)]
pub struct Library {
    pub last: Option<SearchCriteria>,
    pub named: BTreeMap<String, SearchCriteria>,
}
pub struct SearchStore {
    base: SessionStore,
}
impl SearchStore {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            base: SessionStore::new(directory),
        }
    }
    pub fn default_location() -> Result<Self, Error> {
        Ok(Self {
            base: SessionStore::default_location()?,
        })
    }
    pub fn path(&self) -> PathBuf {
        self.base.directory.join("searches.json")
    }
    pub fn load(&self) -> Result<Library, Error> {
        if !self.base.check_directory()? {
            return Ok(Library::default());
        }
        let metadata = match fs::symlink_metadata(self.path()) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Library::default()),
            Err(error) => return Err(error.into()),
        };
        check_regular_file(&metadata)?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options.open(self.path())?;
        let metadata = file.metadata()?;
        check_regular_file(&metadata)?;
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(StorageError::UnsafePermissions.into());
        }
        let mut bytes = vec![];
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(1) => (),
            Some(_) => return Err(StorageError::UnsupportedVersion.into()),
            None => return Err(invalid()),
        }
        let stored: Stored = serde_json::from_value(value).map_err(|_| invalid())?;
        if stored.named.len() > MAX_NAMES {
            return Err(invalid());
        }
        let named = stored
            .named
            .into_iter()
            .map(|(name, criteria)| {
                check_name(&name).map_err(|_| invalid())?;
                Ok((name, criteria.into_criteria()?))
            })
            .collect::<Result<_, Error>>()?;
        Ok(Library {
            last: stored.last.map(CriteriaDto::into_criteria).transpose()?,
            named,
        })
    }
    pub fn save_last(&self, criteria: SearchCriteria) -> Result<(), Error> {
        let _lock = self.mutation_lock()?;
        let mut library = self.load()?;
        library.last = Some(criteria);
        self.write(library)
    }
    pub fn save_named(&self, name: &str, criteria: SearchCriteria) -> Result<(), Error> {
        check_name(name)?;
        let _lock = self.mutation_lock()?;
        let mut library = self.load()?;
        library.named.insert(name.into(), criteria);
        if library.named.len() > MAX_NAMES {
            return Err(Error::TooMany);
        }
        self.write(library)
    }
    pub fn delete_named(&self, name: &str) -> Result<(), Error> {
        check_name(name)?;
        let _lock = self.mutation_lock()?;
        let mut library = self.load()?;
        if library.named.remove(name).is_none() {
            return Err(Error::NotFound);
        }
        self.write(library)
    }
    fn mutation_lock(&self) -> Result<fs::File, Error> {
        if !self.base.check_directory()? {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(&self.base.directory)?;
        }
        if !self.base.check_directory()? {
            return Err(StorageError::UnsafePath.into());
        }
        let path = self.base.directory.join("searches.lock");
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            check_regular_file(&metadata)?;
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options.open(path)?;
        check_regular_file(&file.metadata()?)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
                return Err(StorageError::UnsafePermissions.into());
            }
            // SAFETY: the owned file keeps this valid descriptor alive throughout the call.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
        // Closing this file releases the advisory lock after the read/modify/atomic-write cycle.
        Ok(file)
    }
    fn write(&self, library: Library) -> Result<(), Error> {
        let stored = Stored {
            version: 1,
            last: library.last.as_ref().map(CriteriaDto::from_criteria),
            named: library
                .named
                .iter()
                .map(|(name, criteria)| (name.clone(), CriteriaDto::from_criteria(criteria)))
                .collect(),
        };
        let mut bytes = serde_json::to_vec_pretty(&stored).map_err(|_| invalid())?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        if !self.base.check_directory()? {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(&self.base.directory)?;
        }
        if !self.base.check_directory()? {
            return Err(StorageError::UnsafePath.into());
        }
        match fs::symlink_metadata(self.path()) {
            Ok(metadata) => check_regular_file(&metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        atomic_write(&self.base.directory, &self.path(), |file| {
            file.write_all(&bytes)?;
            Ok(())
        })?;
        Ok(())
    }
}
