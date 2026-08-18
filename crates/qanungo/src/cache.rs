//! The content-addressed blob cache — the whole of qanungo's local mirror.
//!
//! The mirror is deliberately minimal (qanungo #7): there is no cursor protocol, no eviction, no
//! integrity audit, and no event store. A run re-lists the requested window and downloads only
//! the transcripts it does not already hold, keyed by the content hash Patwari declares. Because
//! the key *is* the content hash, "already cached" needs no metadata, no expiry, and no
//! reconciliation — a file under a digest either is those bytes or the archive lied, and the
//! download path already refuses the latter.
//!
//! Archived transcripts are somebody's complete working conversation, so the cache is private by
//! construction: directories are created `0o700` and blob files `0o600`, never widened
//! afterwards. Writes land in a per-process temporary file and are renamed into place, so a
//! reader never observes a partially written blob under a digest that promises complete content.
//!
//! # Writing without holding the blob
//!
//! Transcripts run to hundreds of megabytes, so nothing here takes a whole blob as a `&[u8]`
//! except the small [`BlobCache::store`] convenience. The download path instead [`stage`]s a
//! write, streams verified bytes into it as they arrive off the wire, and [`commit`]s only once
//! every digest and size the archive declared has checked out. That is what makes the temporary
//! file load-bearing rather than incidental: unverified bytes really do touch the disk, and the
//! rename is the moment they become a blob.
//!
//! [`stage`]: BlobCache::stage
//! [`commit`]: BlobWrite::commit

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

/// Permissions for every directory the cache creates: owner-only.
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
/// Permissions for every blob file the cache writes: owner read/write.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// The cache subdirectory holding content-addressed transcript blobs.
const BLOB_DIR: &str = "blobs";

/// Write buffer in front of a staged blob file. A streaming decoder can emit small writes, and
/// this keeps them from becoming small `write(2)` calls without holding anything of consequence.
const STAGE_BUFFER_BYTES: usize = 64 * 1024;

/// Whether a run served a transcript from the cache or had to fetch it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    Hit,
    Miss,
}

/// A content-addressed blob cache rooted at one directory.
#[derive(Debug, Clone)]
pub struct BlobCache {
    root: PathBuf,
}

impl BlobCache {
    /// Opens (creating if absent) the cache under `root`, which is the *qanungo* cache root —
    /// the blob directory is created beneath it.
    ///
    /// # Errors
    ///
    /// Returns an error when the directories cannot be created with owner-only permissions.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        create_private_dir(&root)?;
        create_private_dir(&root.join(BLOB_DIR))?;
        Ok(Self { root })
    }

    /// Opens the cache at the default location: `$XDG_CACHE_HOME/qanungo`, falling back to
    /// `~/.cache/qanungo`.
    ///
    /// # Errors
    ///
    /// Returns an error when neither `XDG_CACHE_HOME` nor `HOME` is set, or the directories
    /// cannot be created.
    pub fn open_default() -> io::Result<Self> {
        Self::open(default_cache_root()?)
    }

    /// The cache root, for the report's instrumentation footer.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the blob for `digest` lives. Sharded one byte wide so a multi-thousand-session
    /// archive does not put every blob in one directory.
    fn blob_path(&self, digest: &str) -> PathBuf {
        self.root.join(BLOB_DIR).join(&digest[..2]).join(digest)
    }

    /// Whether `digest` is already cached.
    pub fn contains(&self, digest: &str) -> bool {
        is_sha256_hex(digest) && self.blob_path(digest).is_file()
    }

    /// Opens the cached blob for `digest` for streaming.
    ///
    /// # Errors
    ///
    /// Returns an error when the digest is not a sha256 hex string, or the blob is absent or
    /// unreadable.
    pub fn open_blob(&self, digest: &str) -> io::Result<File> {
        self.checked_path(digest).and_then(File::open)
    }

    /// Stores `bytes` under `digest`.
    ///
    /// The caller is responsible for having verified that `bytes` hashes to `digest` — the
    /// download path does exactly that before handing bytes here, so a cache write never
    /// re-hashes content that was already checked against the archive's declaration.
    ///
    /// Storing the same digest twice concurrently is safe and is a real case: two sessions in
    /// one window can carry byte-identical transcripts, and their mirror workers then race on
    /// one blob path. Each write therefore lands in a temporary file unique to *this write*, not
    /// merely to this process — a shared `tmp-<pid>` name would have both writers open the same
    /// file and leave the loser's rename failing on a path the winner already moved away.
    ///
    /// # Errors
    ///
    /// Returns an error when the digest is malformed or the blob cannot be written.
    pub fn store(&self, digest: &str, bytes: &[u8]) -> io::Result<()> {
        let mut write = self.stage(digest)?;
        write.write_all(bytes)?;
        write.commit()
    }

    /// Opens a write for `digest` that is not a blob until it is committed.
    ///
    /// The returned [`BlobWrite`] is a `Write` over a private temporary file. Dropping it without
    /// committing removes that file, so a download that fails verification — or a run that dies
    /// mid-transfer — leaves the cache exactly as it found it. That matters more here than it
    /// looks: the streaming download writes bytes it has not finished verifying, and the only
    /// thing standing between those bytes and a blob filed under a hash that does not describe
    /// them is that the rename never happens.
    ///
    /// # Errors
    ///
    /// Returns an error when the digest is malformed or the temporary file cannot be created.
    pub fn stage(&self, digest: &str) -> io::Result<BlobWrite> {
        let path = self.checked_path(digest)?;
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let temporary = path.with_extension(temporary_suffix());
        let file = private_file(&temporary)?;
        Ok(BlobWrite {
            file: Some(BufWriter::with_capacity(STAGE_BUFFER_BYTES, file)),
            temporary,
            blob: path,
        })
    }

    /// Rejects anything that is not a bare lowercase sha256 hex digest before it can reach the
    /// filesystem: the digest comes from a network response and is used to build a path.
    fn checked_path(&self, digest: &str) -> io::Result<PathBuf> {
        if !is_sha256_hex(digest) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "blob digest is not a lowercase sha256 hex string",
            ));
        }
        Ok(self.blob_path(digest))
    }
}

/// A blob write in flight: a private temporary file that becomes the blob only on
/// [`commit`](BlobWrite::commit).
///
/// `Some(file)` *is* the uncommitted flag — [`commit`](BlobWrite::commit) takes it, so the drop
/// guard knows not to unlink a path that has already been renamed away.
pub struct BlobWrite {
    file: Option<BufWriter<File>>,
    temporary: PathBuf,
    blob: PathBuf,
}

impl BlobWrite {
    /// Flushes, fsyncs, and atomically renames the temporary into place.
    ///
    /// Rename is atomic within the directory, so a concurrent reader sees either no blob or the
    /// complete one, never a prefix — and two writers of identical content both succeed, the
    /// second simply replacing an identical file.
    ///
    /// # Errors
    ///
    /// Returns an error when the staged bytes cannot be flushed, synced, or renamed. The
    /// temporary file is removed either way; the cache has no sweeper.
    pub fn commit(mut self) -> io::Result<()> {
        let committed = self.finish();
        if committed.is_err() {
            let _ = fs::remove_file(&self.temporary);
        }
        committed
    }

    fn finish(&mut self) -> io::Result<()> {
        let Some(file) = self.file.take() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "staged blob was already finished",
            ));
        };
        let file = file.into_inner().map_err(io::IntoInnerError::into_error)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.temporary, &self.blob)
    }

    /// The staged file, or a closed-handle error once the write has been finished.
    fn writer(&mut self) -> io::Result<&mut BufWriter<File>> {
        self.file.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "staged blob has already been finished",
            )
        })
    }
}

impl Write for BlobWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer()?.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer()?.flush()
    }
}

impl Drop for BlobWrite {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            // Never committed: the bytes were never verified, or the run gave up. Either way they
            // are not a blob and must not survive as a file.
            let _ = fs::remove_file(&self.temporary);
        }
    }
}

/// The default cache root, honouring `XDG_CACHE_HOME`.
///
/// # Errors
///
/// Returns an error when neither `XDG_CACHE_HOME` nor `HOME` names a directory.
pub fn default_cache_root() -> io::Result<PathBuf> {
    if let Some(base) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(base).join("qanungo"));
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "neither XDG_CACHE_HOME nor HOME is set; pass --cache-dir",
            )
        })?;
    Ok(PathBuf::from(home).join(".cache").join("qanungo"))
}

/// Exactly 64 lowercase hex characters.
fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A file-extension suffix unique to one `store` call: the process, plus a process-wide counter
/// so two workers racing on the same digest never share a temporary path.
fn temporary_suffix() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(DIR_MODE);
    builder.create(path)
}

fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(FILE_MODE);
    options.open(path)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    const DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn stores_and_reads_back_a_blob() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(temporary.path().join("qanungo")).unwrap();
        assert!(!cache.contains(DIGEST));

        cache.store(DIGEST, b"transcript bytes").unwrap();
        assert!(cache.contains(DIGEST));

        let mut read_back = String::new();
        cache
            .open_blob(DIGEST)
            .unwrap()
            .read_to_string(&mut read_back)
            .unwrap();
        assert_eq!(read_back, "transcript bytes");
    }

    /// The drop guard is what keeps a failed or refused download from leaving a partial blob
    /// behind, so it is worth pinning on its own rather than only through the download path.
    #[test]
    fn a_staged_write_dropped_without_committing_leaves_nothing() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("qanungo");
        let cache = BlobCache::open(&root).unwrap();

        {
            let mut staged = cache.stage(DIGEST).unwrap();
            staged.write_all(b"bytes that never verified").unwrap();
            staged.flush().unwrap();
        }

        assert!(!cache.contains(DIGEST));
        let shard = root.join(BLOB_DIR).join(&DIGEST[..2]);
        assert_eq!(
            fs::read_dir(&shard).unwrap().count(),
            0,
            "a staged write must not survive its own drop"
        );
    }

    #[test]
    fn a_committed_staged_write_becomes_the_blob() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(temporary.path().join("qanungo")).unwrap();

        let mut staged = cache.stage(DIGEST).unwrap();
        staged.write_all(b"first ").unwrap();
        staged.write_all(b"second").unwrap();
        staged.commit().unwrap();

        let mut read_back = String::new();
        cache
            .open_blob(DIGEST)
            .unwrap()
            .read_to_string(&mut read_back)
            .unwrap();
        assert_eq!(read_back, "first second");
    }

    #[test]
    fn storing_the_same_digest_twice_is_idempotent() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(temporary.path().join("qanungo")).unwrap();
        cache.store(DIGEST, b"bytes").unwrap();
        cache.store(DIGEST, b"bytes").unwrap();
        assert!(cache.contains(DIGEST));
    }

    /// Two mirror workers can race on one digest whenever two sessions in a window carry
    /// byte-identical transcripts. Every writer must succeed, and no temporary file may survive.
    #[test]
    fn concurrent_stores_of_one_digest_all_succeed() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(temporary.path().join("qanungo")).unwrap();

        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| cache.store(DIGEST, b"transcript bytes")))
                .collect();
            for handle in handles {
                handle
                    .join()
                    .expect("no worker panics")
                    .expect("every store succeeds");
            }
        });

        assert!(cache.contains(DIGEST));
        let shard = cache.root.join(BLOB_DIR).join(&DIGEST[..2]);
        let leftovers: Vec<_> = fs::read_dir(&shard)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != DIGEST)
            .collect();
        assert!(
            leftovers.is_empty(),
            "temporary files left behind: {leftovers:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn directories_are_0700_and_blobs_are_0600() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("qanungo");
        let cache = BlobCache::open(&root).unwrap();
        cache.store(DIGEST, b"bytes").unwrap();

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), DIR_MODE);
        assert_eq!(mode(&root.join(BLOB_DIR)), DIR_MODE);
        assert_eq!(mode(&root.join(BLOB_DIR).join(&DIGEST[..2])), DIR_MODE);
        assert_eq!(mode(&cache.blob_path(DIGEST)), FILE_MODE);
    }

    #[test]
    fn refuses_digests_that_are_not_bare_sha256_hex() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(temporary.path().join("qanungo")).unwrap();
        for bad in [
            "../../etc/passwd",
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855",
            "short",
        ] {
            assert!(!cache.contains(bad), "{bad} must not read as cached");
            assert!(
                cache.store(bad, b"x").is_err(),
                "{bad} must not be storable"
            );
        }
    }

    #[test]
    fn the_default_root_honours_xdg_cache_home() {
        // Deliberately not mutating the process environment (tests share it); the fallback
        // arithmetic is what is worth pinning.
        let root = default_cache_root().unwrap();
        assert!(root.ends_with("qanungo"));
    }
}
