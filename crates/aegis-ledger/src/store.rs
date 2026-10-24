//! Where ledger events and records are kept (ADR 0003). The issue
//! lifecycle only sees [`LedgerStore`]; JSON files are one implementation,
//! Postgres (`aegis-store`, #22) the other. [`copy_ledger`] moves a ledger
//! between them.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tokio::io::AsyncWriteExt;

use crate::{IssueId, IssueRecord, LedgerError, LedgerEvent};

pub trait LedgerStore {
    /// Durably appends one event. Called after every model call and gate run.
    fn append(
        &self,
        issue: &IssueId,
        event: &LedgerEvent,
    ) -> impl Future<Output = Result<(), LedgerError>> + Send;

    /// All events of an issue in append order; empty if none.
    fn events(
        &self,
        issue: &IssueId,
    ) -> impl Future<Output = Result<Vec<LedgerEvent>, LedgerError>> + Send;

    /// Stores the summary record, replacing any previous one.
    fn save_record(
        &self,
        record: &IssueRecord,
    ) -> impl Future<Output = Result<(), LedgerError>> + Send;

    /// Every issue with at least one event, in id order (KPI reports).
    fn issues(&self) -> impl Future<Output = Result<Vec<IssueId>, LedgerError>> + Send;
}

/// `<root>/<issue>.events.jsonl` (one event per line) and
/// `<root>/<issue>.json` (the summary record).
#[derive(Debug, Clone)]
pub struct JsonFileLedger {
    root: PathBuf,
}

impl JsonFileLedger {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn events_path(&self, issue: &IssueId) -> PathBuf {
        self.root.join(format!("{issue}.events.jsonl"))
    }

    pub fn record_path(&self, issue: &IssueId) -> PathBuf {
        self.root.join(format!("{issue}.json"))
    }

    async fn create_root(&self) -> Result<(), LedgerError> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(LedgerError::io(&self.root))
    }
}

impl LedgerStore for JsonFileLedger {
    async fn append(&self, issue: &IssueId, event: &LedgerEvent) -> Result<(), LedgerError> {
        event.ensure_finite()?;
        let mut line = serde_json::to_vec(event).map_err(|e| LedgerError::Encode(e.to_string()))?;
        line.push(b'\n');
        self.create_root().await?;
        let path = self.events_path(issue);
        let existed = drop_torn_tail(&path).await?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(LedgerError::io(&path))?;
        file.write_all(&line)
            .await
            .map_err(LedgerError::io(&path))?;
        file.sync_data().await.map_err(LedgerError::io(&path))?;
        if existed {
            Ok(())
        } else {
            sync_dir(&self.root).await
        }
    }

    async fn events(&self, issue: &IssueId) -> Result<Vec<LedgerEvent>, LedgerError> {
        let path = self.events_path(issue);
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => parse_events(&path, &text),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(LedgerError::io(&path)(e)),
        }
    }

    async fn save_record(&self, record: &IssueRecord) -> Result<(), LedgerError> {
        let mut json =
            serde_json::to_vec_pretty(record).map_err(|e| LedgerError::Encode(e.to_string()))?;
        json.push(b'\n');
        self.create_root().await?;
        let path = self.record_path(&record.issue_id);
        let tmp = path.with_extension("json.tmp");
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .map_err(LedgerError::io(&tmp))?;
        file.write_all(&json).await.map_err(LedgerError::io(&tmp))?;
        file.sync_all().await.map_err(LedgerError::io(&tmp))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(LedgerError::io(&path))?;
        sync_dir(&self.root).await
    }

    async fn issues(&self) -> Result<Vec<IssueId>, LedgerError> {
        let mut dir = match tokio::fs::read_dir(&self.root).await {
            Ok(dir) => dir,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(LedgerError::io(&self.root)(e)),
        };
        let mut ids = Vec::new();
        while let Some(entry) = dir
            .next_entry()
            .await
            .map_err(LedgerError::io(&self.root))?
        {
            let name = entry.file_name();
            if let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".events.jsonl")) {
                ids.push(IssueId::parse(id)?);
            }
        }
        ids.sort();
        Ok(ids)
    }
}

/// Cuts a fragment left after the last newline by a crash mid-append, so
/// the next event does not glue onto it. Returns whether the file exists.
async fn drop_torn_tail(path: &Path) -> Result<bool, LedgerError> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(LedgerError::io(path)(e)),
    };
    if bytes.last().is_none_or(|&b| b == b'\n') {
        return Ok(true);
    }
    let keep = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .await
        .map_err(LedgerError::io(path))?;
    file.set_len(keep as u64)
        .await
        .map_err(LedgerError::io(path))?;
    file.sync_data().await.map_err(LedgerError::io(path))?;
    Ok(true)
}

/// Persists a new or renamed directory entry.
async fn sync_dir(dir: &Path) -> Result<(), LedgerError> {
    #[cfg(unix)]
    {
        let handle = tokio::fs::File::open(dir)
            .await
            .map_err(LedgerError::io(dir))?;
        handle.sync_all().await.map_err(LedgerError::io(dir))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// A crash mid-append can leave a last line without its newline; that
/// fragment is an event that was never acknowledged, so it is dropped.
fn parse_events(path: &Path, text: &str) -> Result<Vec<LedgerEvent>, LedgerError> {
    let complete = match text.rfind('\n') {
        Some(end) => &text[..end],
        None => "",
    };
    complete
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            serde_json::from_str(line).map_err(|e| LedgerError::Corrupt {
                path: path.to_owned(),
                line: i + 1,
                message: e.to_string(),
            })
        })
        .collect()
}

/// What [`copy_ledger`] did, by issue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyReport {
    /// New in the target, or with events added at its end.
    pub copied: Vec<IssueId>,
    /// Already the same in the target.
    pub unchanged: Vec<IssueId>,
}

/// Copies every issue's events from `from` into `to`, then saves the
/// record they fold into. Safe to run again: an issue whose events in `to`
/// are the same as in `from`, or the start of them (an earlier copy that
/// stopped, or a ledger that grew since), only gets what is missing, and
/// its record is saved again in case that copy stopped before it. An issue
/// whose events differ is an error, before anything of it is written.
/// Nothing else may write the same issues while it runs.
pub async fn copy_ledger<F, T>(from: &F, to: &T) -> Result<CopyReport, LedgerError>
where
    F: LedgerStore + Sync,
    T: LedgerStore + Sync,
{
    let mut report = CopyReport::default();
    for issue in from.issues().await? {
        let events = from.events(&issue).await?;
        let have = to.events(&issue).await?;
        if !events.starts_with(&have) {
            return Err(LedgerError::Store(format!(
                "{issue}: the target's {} events are not the start of the source's {}",
                have.len(),
                events.len()
            )));
        }
        for event in &events[have.len()..] {
            to.append(&issue, event).await?;
        }
        to.save_record(&IssueRecord::from_events(issue.clone(), &events))
            .await?;
        if have.len() == events.len() {
            report.unchanged.push(issue);
        } else {
            report.copied.push(issue);
        }
    }
    Ok(report)
}

/// In-memory store for tests of code that writes to the ledger.
#[derive(Debug, Default)]
pub struct MemoryLedger {
    events: Mutex<BTreeMap<IssueId, Vec<LedgerEvent>>>,
    records: Mutex<BTreeMap<IssueId, IssueRecord>>,
}

impl MemoryLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, issue: &IssueId) -> Option<IssueRecord> {
        lock(&self.records).get(issue).cloned()
    }
}

impl LedgerStore for MemoryLedger {
    async fn append(&self, issue: &IssueId, event: &LedgerEvent) -> Result<(), LedgerError> {
        event.ensure_finite()?;
        lock(&self.events)
            .entry(issue.clone())
            .or_default()
            .push(event.clone());
        Ok(())
    }

    async fn events(&self, issue: &IssueId) -> Result<Vec<LedgerEvent>, LedgerError> {
        Ok(lock(&self.events).get(issue).cloned().unwrap_or_default())
    }

    async fn save_record(&self, record: &IssueRecord) -> Result<(), LedgerError> {
        lock(&self.records).insert(record.issue_id.clone(), record.clone());
        Ok(())
    }

    async fn issues(&self) -> Result<Vec<IssueId>, LedgerError> {
        Ok(lock(&self.events).keys().cloned().collect())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A panic while holding the lock cannot leave the maps half-updated.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::{IssueKind, Outcome};

    fn temp_root() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "aegis-ledger-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn issue() -> IssueId {
        IssueId::parse("GH-7").unwrap()
    }

    fn classified() -> LedgerEvent {
        LedgerEvent::Classified {
            kind: IssueKind::Sonar,
        }
    }

    fn finished() -> LedgerEvent {
        LedgerEvent::Finished {
            outcome: Outcome::StoppedSteps,
            wall_time_s: 1.0,
        }
    }

    #[tokio::test]
    async fn file_ledger_appends_and_reads_back() {
        let root = temp_root();
        let store = JsonFileLedger::new(&root);
        assert!(store.events(&issue()).await.unwrap().is_empty());

        store.append(&issue(), &classified()).await.unwrap();
        store.append(&issue(), &finished()).await.unwrap();

        assert_eq!(
            store.events(&issue()).await.unwrap(),
            vec![classified(), finished()]
        );
        let text = std::fs::read_to_string(store.events_path(&issue())).unwrap();
        assert_eq!(text.lines().count(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn ids_differing_only_in_case_share_one_ledger() {
        let root = temp_root();
        let store = JsonFileLedger::new(&root);
        let lower = IssueId::parse("gh-7").unwrap();
        store.append(&lower, &classified()).await.unwrap();
        store.append(&issue(), &finished()).await.unwrap();

        assert_eq!(store.events_path(&lower), store.events_path(&issue()));
        assert_eq!(
            store.events(&lower).await.unwrap(),
            vec![classified(), finished()]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn file_ledger_drops_a_torn_last_line() {
        let root = temp_root();
        let store = JsonFileLedger::new(&root);
        store.append(&issue(), &classified()).await.unwrap();
        let path = store.events_path(&issue());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(r#"{"event":"finished","outc"#);
        std::fs::write(&path, text).unwrap();

        assert_eq!(store.events(&issue()).await.unwrap(), vec![classified()]);

        store.append(&issue(), &finished()).await.unwrap();
        assert_eq!(
            store.events(&issue()).await.unwrap(),
            vec![classified(), finished()],
            "the fragment was cut, not glued onto the next event"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn file_ledger_rejects_non_finite_numbers() {
        let root = temp_root();
        let store = JsonFileLedger::new(&root);
        let event = LedgerEvent::Finished {
            outcome: Outcome::PrOpened,
            wall_time_s: f64::NAN,
        };
        let err = store.append(&issue(), &event).await.unwrap_err();
        assert!(
            matches!(
                err,
                LedgerError::NonFinite {
                    field: "wall_time_s",
                    ..
                }
            ),
            "{err}"
        );
        assert!(store.events(&issue()).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn file_ledger_reports_a_corrupt_complete_line() {
        let root = temp_root();
        let store = JsonFileLedger::new(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            store.events_path(&issue()),
            "{\"event\":\"classified\",\"kind\":\"bug\"}\nnot json\n",
        )
        .unwrap();

        let err = store.events(&issue()).await.unwrap_err();
        assert!(matches!(err, LedgerError::Corrupt { line: 2, .. }), "{err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn file_ledger_saves_record_as_json() {
        let root = temp_root();
        let store = JsonFileLedger::new(&root);
        let record = IssueRecord::from_events(issue(), &[classified(), finished()]);

        store.save_record(&record).await.unwrap();
        store.save_record(&record).await.unwrap();

        let text = std::fs::read_to_string(store.record_path(&issue())).unwrap();
        let back: IssueRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(back, record);
        let names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["GH-7.json"], "no temp file left behind");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn memory_ledger_keeps_events_and_records() {
        let store = MemoryLedger::new();
        store.append(&issue(), &classified()).await.unwrap();
        assert_eq!(store.events(&issue()).await.unwrap(), vec![classified()]);
        assert_eq!(store.record(&issue()), None);

        let record = IssueRecord::from_events(issue(), &[classified()]);
        store.save_record(&record).await.unwrap();
        assert_eq!(store.record(&issue()), Some(record));
    }

    #[tokio::test]
    async fn copy_resumes_skips_and_refuses_a_diverging_ledger() {
        let from = MemoryLedger::new();
        let other = IssueId::parse("GH-8").unwrap();
        for event in [classified(), finished()] {
            from.append(&issue(), &event).await.unwrap();
        }
        from.append(&other, &classified()).await.unwrap();
        let root = temp_root();
        let to = JsonFileLedger::new(&root);
        to.append(&issue(), &classified()).await.unwrap();

        let report = copy_ledger(&from, &to).await.unwrap();
        assert_eq!(report.copied, vec![issue(), other.clone()]);
        assert_eq!(
            to.events(&issue()).await.unwrap(),
            vec![classified(), finished()],
            "the missing tail only"
        );
        let saved = std::fs::read_to_string(to.record_path(&issue())).unwrap();
        let saved: IssueRecord = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved.outcome, Some(Outcome::StoppedSteps));

        // A copy that stopped between the last event and the record.
        std::fs::remove_file(to.record_path(&issue())).unwrap();
        let again = copy_ledger(&from, &to).await.unwrap();
        assert_eq!(again.unchanged, vec![issue(), other.clone()]);
        assert!(again.copied.is_empty());
        assert!(
            to.record_path(&issue()).exists(),
            "the record is saved again"
        );

        let diverged = MemoryLedger::new();
        diverged.append(&other, &finished()).await.unwrap();
        let err = copy_ledger(&diverged, &to).await.unwrap_err();
        assert!(matches!(err, LedgerError::Store(_)), "{err}");
        assert_eq!(to.events(&other).await.unwrap(), vec![classified()]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn json_files_list_their_issues() {
        let root = std::env::temp_dir().join(format!("aegis-ledger-issues-{}", std::process::id()));
        let store = JsonFileLedger::new(&root);
        assert!(store.issues().await.unwrap().is_empty(), "no directory yet");
        for id in ["b-2", "a-1"] {
            let event = LedgerEvent::Classified {
                kind: crate::IssueKind::Bug,
            };
            store
                .append(&IssueId::parse(id).unwrap(), &event)
                .await
                .unwrap();
        }
        let ids: Vec<String> = store
            .issues()
            .await
            .unwrap()
            .iter()
            .map(|i| i.to_string())
            .collect();
        assert_eq!(ids, ["A-1", "B-2"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
