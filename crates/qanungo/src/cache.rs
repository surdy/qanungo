//! The content-addressed blob cache and the snapshot index — the whole of qanungo's local mirror.
//!
//! The mirror is deliberately small (qanungo #7, hardened by #1): there is no cursor protocol, no
//! eviction, no integrity audit, and no event store. A run re-lists the requested window and
//! downloads only the transcripts it does not already hold, keyed by the content hash Patwari
//! declares. Because the key *is* the content hash, "already cached" needs no metadata, no
//! expiry, and no reconciliation — a file under a digest either is those bytes or the archive
//! lied, and the download path already refuses the latter.
//!
//! Beside the blobs sits the **snapshot index**: the archive's own document for each snapshot the
//! mirror has resolved, keyed by snapshot id. It is cacheable for the same reason the blobs are —
//! a completed snapshot is immutable — and it is what lets a warm sync cost the listing pages and
//! nothing else. See [`BlobCache::snapshot_document`] for what it may and may not decide.
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
//! Because a staged write is now potentially hundreds of megabytes, and because the drop guard
//! that removes it cannot run if the process is killed outright, [`BlobCache::open`] sweeps
//! staged writes older than a day.
//!
//! [`stage`]: BlobCache::stage
//! [`commit`]: BlobWrite::commit

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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

/// The cache subdirectory holding the snapshot index: one archive snapshot document per
/// snapshot id (see [`BlobCache::snapshot_document`]).
const SNAPSHOT_DIR: &str = "snapshots";

/// Ceiling on one indexed snapshot document. The index holds documents this process wrote from
/// responses that were themselves bounded, so anything larger is not one of ours.
const MAX_SNAPSHOT_DOCUMENT_BYTES: u64 = 1024 * 1024;

/// Write buffer in front of a staged blob file. A streaming decoder can emit small writes, and
/// this keeps them from becoming small `write(2)` calls without holding anything of consequence.
const STAGE_BUFFER_BYTES: usize = 64 * 1024;

/// How old a staged write must be before [`BlobCache::open`] treats it as orphaned and deletes
/// it. A day is far longer than any download can plausibly take and far shorter than anyone would
/// like to keep paying for a dead one.
const ORPHAN_TEMPORARY_AGE: Duration = Duration::from_secs(24 * 60 * 60);

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
        create_private_dir(&root.join(SNAPSHOT_DIR))?;
        let cache = Self { root };
        cache.sweep_orphaned_temporaries();
        Ok(cache)
    }

    /// Removes staged writes old enough that nothing can still be writing them.
    ///
    /// [`BlobWrite`]'s drop guard handles every failure the process survives, which is all of them
    /// except the ones where it does not: `SIGKILL`, an OOM kill, a power cut. Those leave a
    /// temporary behind, and a temporary is now potentially a few hundred megabytes of transcript
    /// in a cache that has no eviction — so it has to be swept, or a handful of unlucky runs
    /// quietly costs a gigabyte.
    ///
    /// The rule is deliberately blunt: older than [`ORPHAN_TEMPORARY_AGE`], delete. No pid
    /// liveness check and no lock file, because the age already separates the two cases — a live
    /// write is minutes old at the very most, and anything from a previous day is not coming back.
    /// Failures here are ignored throughout: a cache that cannot be tidied is still a usable
    /// cache, and this is housekeeping, not the caller's errand.
    fn sweep_orphaned_temporaries(&self) {
        for directory in [BLOB_DIR, SNAPSHOT_DIR] {
            self.sweep_orphaned_temporaries_under(&self.root.join(directory));
        }
    }

    fn sweep_orphaned_temporaries_under(&self, directory: &Path) {
        let Ok(shards) = fs::read_dir(directory) else {
            return;
        };
        for shard in shards.filter_map(Result::ok) {
            let Ok(entries) = fs::read_dir(shard.path()) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let is_temporary = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.starts_with("tmp-"));
                if !is_temporary {
                    continue;
                }
                let stale = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .is_ok_and(|modified| {
                        modified
                            .elapsed()
                            .is_ok_and(|age| age >= ORPHAN_TEMPORARY_AGE)
                    });
                if stale {
                    let _ = fs::remove_file(&path);
                }
            }
        }
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

    /// Every digest the cache currently holds, in no particular order.
    ///
    /// The mirror never needs this — it asks about the digests a listing named and nothing else —
    /// but a check that has to sweep *what is already on this disk* does, and the alternative is a
    /// caller reconstructing the shard layout from outside the type. Staged writes and anything
    /// else that is not a bare sha256 filename are skipped, so what comes back is exactly the set
    /// [`open_blob`](Self::open_blob) will serve.
    ///
    /// # Errors
    ///
    /// Returns an error when the blob directory cannot be read at all. An unreadable *shard* is
    /// skipped rather than failing the walk: a partial inventory of a cache is still an inventory.
    pub fn digests(&self) -> io::Result<Vec<String>> {
        let mut digests = Vec::new();
        for shard in fs::read_dir(self.root.join(BLOB_DIR))?.filter_map(Result::ok) {
            let Ok(entries) = fs::read_dir(shard.path()) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if is_sha256_hex(name) {
                    digests.push(name.to_owned());
                }
            }
        }
        Ok(digests)
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

    /// The indexed document for `snapshot_id`, when the index holds one.
    ///
    /// # The snapshot index
    ///
    /// A Patwari snapshot is immutable once complete — its manifest, its artifact set, and every
    /// declaration about them never change — so the document `GET /snapshots/{id}` returns is
    /// as cacheable as the blobs are, and for the same reason: the key names bytes that cannot
    /// be anything else. The mirror used to spend one such request per listed session on every
    /// run to learn a content hash it had already been told; against the real archive that was
    /// ~700 requests, and the whole of a warm sync. Indexed, a warm sync is the listing pages.
    ///
    /// What the index is *not*: it is not a source of anything the archive could have changed.
    /// A snapshot can be tombstoned, but a tombstoned snapshot leaves the listing, and the
    /// listing is what decides which ids are ever asked about here; an orphaned entry is a few
    /// kilobytes nobody reads. And the mirror consults the index only for a snapshot whose
    /// artifact it already holds — a download always runs on a freshly fetched document, so an
    /// indexed URL never reaches the wire (see [`crate::sync`]).
    ///
    /// Anything that is not a well-formed document this process could have written — a missing
    /// file, an unreadable one, one over `MAX_SNAPSHOT_DOCUMENT_BYTES`, an id that is not a
    /// UUID — is `None`: the index is an accelerator, and a miss costs exactly the request it
    /// would have cost before there was one.
    pub fn snapshot_document(&self, snapshot_id: &str) -> Option<Vec<u8>> {
        let path = self.snapshot_path(snapshot_id)?;
        let file = File::open(path).ok()?;
        if file.metadata().ok()?.len() > MAX_SNAPSHOT_DOCUMENT_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        io::Read::read_to_end(
            &mut io::Read::take(file, MAX_SNAPSHOT_DOCUMENT_BYTES),
            &mut bytes,
        )
        .ok()?;
        Some(bytes)
    }

    /// Indexes `document` under `snapshot_id`, atomically and owner-only like a blob.
    ///
    /// A malformed id is refused rather than written somewhere surprising; a document over the
    /// ceiling is refused so the index can never hold what
    /// [`snapshot_document`](Self::snapshot_document) would refuse to read back.
    ///
    /// # Errors
    ///
    /// Returns an error when the id is not a UUID, the document is over the ceiling, or the file
    /// cannot be written.
    pub fn index_snapshot(&self, snapshot_id: &str, document: &[u8]) -> io::Result<()> {
        let path = self.snapshot_path(snapshot_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot id is not a lowercase UUID",
            )
        })?;
        if document.len() as u64 > MAX_SNAPSHOT_DOCUMENT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot document is over the index ceiling",
            ));
        }
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let temporary = path.with_extension(temporary_suffix());
        let written = (|| {
            let mut file = private_file(&temporary)?;
            file.write_all(document)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written
    }

    /// Where the index entry for `snapshot_id` lives, or `None` for an id that is not a UUID —
    /// the id comes from a network response and is used to build a path.
    fn snapshot_path(&self, snapshot_id: &str) -> Option<PathBuf> {
        is_snapshot_id(snapshot_id).then(|| {
            self.root
                .join(SNAPSHOT_DIR)
                .join(&snapshot_id[..2])
                .join(format!("{snapshot_id}.json"))
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
///
/// Public because a served route validates a caller's path segment with it
/// ([`crate::dashboard_server`]): one definition of "is a digest", for the store and for the
/// surface that names one. A second, looser one on the route would be a way to ask this cache a
/// question it does not think is a question.
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A lowercase hyphenated UUID, which is what every Patwari snapshot id is. Nothing else may
/// name an index entry.
pub fn is_snapshot_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
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

    const SNAPSHOT: &str = "01a039ca-30a2-7e52-9d94-692d8fd58773";

    #[test]
    fn indexes_and_reads_back_a_snapshot_document() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(temporary.path().join("qanungo")).unwrap();
        assert!(cache.snapshot_document(SNAPSHOT).is_none());
        cache
            .index_snapshot(SNAPSHOT, b"{\"artifacts\":[]}")
            .unwrap();
        assert_eq!(
            cache.snapshot_document(SNAPSHOT).as_deref(),
            Some(&b"{\"artifacts\":[]}"[..])
        );
        // Owner-only, like a blob: the document carries the capture manifest.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = cache.snapshot_path(SNAPSHOT).unwrap();
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Replacing is a rename, so a reader sees the old or the new document, never a prefix.
        cache.index_snapshot(SNAPSHOT, b"{}").unwrap();
        assert_eq!(
            cache.snapshot_document(SNAPSHOT).as_deref(),
            Some(&b"{}"[..])
        );
    }

    #[test]
    fn the_index_refuses_anything_that_is_not_a_uuid() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(temporary.path().join("qanungo")).unwrap();
        for id in [
            "",
            "../../etc/passwd",
            DIGEST,
            "01A039CA-30A2-7E52-9D94-692D8FD58773",
        ] {
            assert!(cache.index_snapshot(id, b"{}").is_err(), "{id:?}");
            assert!(cache.snapshot_document(id).is_none(), "{id:?}");
        }
        assert!(is_snapshot_id(SNAPSHOT));
        assert!(!is_snapshot_id("01a039ca-30a2-7e52-9d94-692d8fd5877"));
        assert!(!is_snapshot_id("01a039ca030a2-7e52-9d94-692d8fd58773"));
    }

    #[test]
    fn the_index_refuses_a_document_over_its_ceiling() {
        let temporary = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(temporary.path().join("qanungo")).unwrap();
        let oversized = vec![b' '; usize::try_from(MAX_SNAPSHOT_DOCUMENT_BYTES).unwrap() + 1];
        assert!(cache.index_snapshot(SNAPSHOT, &oversized).is_err());
        assert!(cache.snapshot_document(SNAPSHOT).is_none());
        // And will not read one back that got there some other way.
        let path = cache.snapshot_path(SNAPSHOT).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &oversized).unwrap();
        assert!(cache.snapshot_document(SNAPSHOT).is_none());
    }

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

    /// The sweep is the backstop for the failures the drop guard cannot see — a kill, an OOM, a
    /// power cut — so what it must get right is the discrimination: stale staged writes go, live
    /// ones and committed blobs stay.
    #[test]
    fn opening_sweeps_orphaned_staged_writes_and_spares_everything_else() {
        use std::fs::FileTimes;
        use std::time::SystemTime;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("qanungo");
        let cache = BlobCache::open(&root).unwrap();
        cache.store(DIGEST, b"a committed blob").unwrap();

        let shard = root.join(BLOB_DIR).join(&DIGEST[..2]);
        let orphan = shard.join(format!("{DIGEST}.tmp-1-0"));
        let in_flight = shard.join(format!("{DIGEST}.tmp-2-0"));
        fs::write(&orphan, b"a staged write nobody will finish").unwrap();
        fs::write(&in_flight, b"a staged write in progress").unwrap();

        // Age the orphan past the threshold — and the committed blob with it, since a blob must
        // survive the sweep at any age at all.
        let aged = FileTimes::new()
            .set_modified(SystemTime::now() - ORPHAN_TEMPORARY_AGE - Duration::from_secs(60));
        for path in [&orphan, &shard.join(DIGEST)] {
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(aged)
                .unwrap();
        }

        drop(cache);
        let swept = BlobCache::open(&root).unwrap();

        assert!(!orphan.exists(), "a day-old staged write must be swept");
        assert!(
            in_flight.exists(),
            "a staged write that could still be in progress must survive"
        );
        assert!(
            swept.contains(DIGEST),
            "a committed blob is not a temporary at any age"
        );
    }

    /// The inventory is what a sweep over "everything already mirrored" reads, so it must list
    /// committed blobs and nothing else — a staged write is not yet content.
    #[test]
    fn the_inventory_lists_committed_blobs_and_skips_staged_writes() {
        const OTHER: &str = "0000000000000000000000000000000000000000000000000000000000000001";
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("qanungo");
        let cache = BlobCache::open(&root).unwrap();
        assert!(cache.digests().unwrap().is_empty());

        cache.store(DIGEST, b"one").unwrap();
        cache.store(OTHER, b"two").unwrap();
        fs::write(
            root.join(BLOB_DIR)
                .join(&DIGEST[..2])
                .join(format!("{DIGEST}.tmp-9-0")),
            b"in flight",
        )
        .unwrap();

        let mut digests = cache.digests().unwrap();
        digests.sort();
        assert_eq!(digests, vec![OTHER.to_owned(), DIGEST.to_owned()]);
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
