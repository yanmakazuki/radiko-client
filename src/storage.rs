//! セッションの読み込みと原子的保存。破損ファイルの保護と保存先の権限検査を行う。
pub mod searches;
use crate::session::MemberSession;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::{
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAX_SESSION_BYTES: u64 = 16 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum StorageError {
    Io(io::ErrorKind),
    Invalid,
    UnsupportedVersion,
    UnsafePermissions,
    UnsafePath,
    NoDataDirectory,
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "セッションファイルの操作に失敗しました ({kind:?})"),
            Self::Invalid => f.write_str("セッションファイルが破損しています"),
            Self::UnsupportedVersion => f.write_str("未対応のセッション保存形式です"),
            Self::UnsafePermissions => {
                f.write_str("セッションのディレクトリは0700、ファイルは0600にしてください")
            }
            Self::UnsafePath => f.write_str(
                "セッション保存先にシンボリックリンクや通常ファイル以外は使用できません",
            ),
            Self::NoDataDirectory => {
                f.write_str("データ保存先を解決できません。--data-dirを指定してください")
            }
        }
    }
}
impl std::error::Error for StorageError {}
impl From<io::Error> for StorageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSession {
    version: u32,
    session: MemberSession,
}

pub struct SessionStore {
    directory: PathBuf,
}

impl SessionStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn default_location() -> Result<Self, StorageError> {
        let dirs = directories::ProjectDirs::from("jp", "radiko-client", "radiko-client")
            .ok_or(StorageError::NoDataDirectory)?;
        Ok(Self::new(dirs.data_local_dir().to_owned()))
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join("session.json")
    }

    pub fn load(&self) -> Result<Option<MemberSession>, StorageError> {
        if !self.check_directory()? {
            return Ok(None);
        }
        let metadata = match fs::symlink_metadata(self.path()) {
            Ok(value) => value,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        check_regular_file(&metadata)?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options.open(self.path())?;
        // 開く前のパスだけでなく、実際に開いたファイルも検査する。
        let metadata = file.metadata()?;
        check_regular_file(&metadata)?;
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(StorageError::UnsafePermissions);
        }
        let mut bytes = Vec::new();
        file.take(MAX_SESSION_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SESSION_BYTES {
            return Err(StorageError::Invalid);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| StorageError::Invalid)?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(1) => (),
            Some(_) => return Err(StorageError::UnsupportedVersion),
            None => return Err(StorageError::Invalid),
        }
        let stored: StoredSession =
            serde_json::from_value(value).map_err(|_| StorageError::Invalid)?;
        if !stored.session.validate() {
            return Err(StorageError::Invalid);
        }
        Ok(Some(stored.session))
    }

    /// 明示的なログイン成功後にだけ呼ぶ。既存ファイルが破損していても置き換える。
    pub fn save(&self, session: MemberSession) -> Result<(), StorageError> {
        if !session.validate() {
            return Err(StorageError::Invalid);
        }
        if !self.check_directory()? {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(&self.directory)?;
        }
        if !self.check_directory()? {
            return Err(StorageError::UnsafePath);
        }
        match fs::symlink_metadata(self.path()) {
            Ok(metadata) => check_regular_file(&metadata)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        let stored = StoredSession {
            version: 1,
            session,
        };
        atomic_write(&self.directory, &self.path(), |file| {
            serde_json::to_writer(&mut *file, &stored).map_err(|_| StorageError::Invalid)?;
            file.write_all(b"\n")?;
            Ok(())
        })
    }

    /// 破損時も通信せずにローカルセッションを削除できる。
    pub fn delete(&self) -> Result<(), StorageError> {
        if !self.check_directory()? {
            return Ok(());
        }
        match fs::remove_file(self.path()) {
            Ok(()) => {
                fs::File::open(&self.directory)?.sync_all()?;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn check_directory(&self) -> Result<bool, StorageError> {
        match fs::symlink_metadata(&self.directory) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(StorageError::UnsafePath);
                }
                #[cfg(unix)]
                if metadata.permissions().mode() & 0o777 != 0o700 {
                    return Err(StorageError::UnsafePermissions);
                }
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

fn check_regular_file(metadata: &fs::Metadata) -> Result<(), StorageError> {
    if metadata.is_file() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(StorageError::UnsafePath)
    }
}

fn atomic_write(
    directory: &Path,
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    // tempfileはUnix上で最初の書き込み前から0600の一時ファイルを作る。
    write(temporary.as_file_mut())?;
    // 内容を同期してから置き換え、最後にディレクトリの変更も同期する。
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|e| StorageError::from(e.error))?;
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_write_preserves_existing_file_and_cleans_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.json");
        fs::write(&path, b"original").unwrap();
        let result = atomic_write(directory.path(), &path, |file| {
            #[cfg(unix)]
            assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
            file.write_all(b"partial")?;
            Err(StorageError::Io(io::ErrorKind::Interrupted))
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
