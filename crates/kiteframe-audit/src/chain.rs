use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use kiteframe_contract::{
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage, RetryClass, Sha256Digest,
};
use kiteframe_provider::{AuditRecord, AuditSink, DurableAuditReceipt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const AUDIT_HASH_DOMAIN: &[u8] = b"kiteframe:audit:v1\0";
const PARTITION_FILE_DOMAIN: &[u8] = b"kiteframe:audit-partition:v1\0";

pub struct FileAuditLedger {
    root: PathBuf,
    partition_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}

impl FileAuditLedger {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, Diagnostic> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)
            .map_err(|_| unavailable("audit ledger directory cannot be created"))?;
        if !root.is_dir() {
            return Err(unavailable("audit ledger root is not a directory"));
        }
        Ok(Self {
            root: root.to_path_buf(),
            partition_locks: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn partition_path(&self, partition: &str) -> Result<PathBuf, Diagnostic> {
        validate_partition(partition)?;
        let mut hasher = Sha256::new();
        hasher.update(PARTITION_FILE_DOMAIN);
        hasher.update(partition.as_bytes());
        let digest = Sha256Digest::from_bytes(hasher.finalize().into());
        Ok(self.root.join(format!("{digest}.audit.jsonl")))
    }

    pub fn verify_partition(&self, partition: &str) -> Result<Vec<VerifiedAuditEntry>, Diagnostic> {
        self.with_partition_lock(partition, || {
            let path = self.partition_path(partition)?;
            if !path.exists() {
                return Ok(Vec::new());
            }
            let mut file = OpenOptions::new()
                .read(true)
                .open(path)
                .map_err(|_| unavailable("audit partition cannot be opened"))?;
            file.lock_shared()
                .map_err(|_| unavailable("audit partition cannot be locked for verification"))?;
            read_and_verify(&mut file, partition)
        })
    }

    fn with_partition_lock<T>(
        &self,
        partition: &str,
        operation: impl FnOnce() -> Result<T, Diagnostic>,
    ) -> Result<T, Diagnostic> {
        validate_partition(partition)?;
        let lock = {
            let mut locks = self
                .partition_locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locks
                .entry(partition.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation()
    }

    fn append_sync(&self, record: AuditRecord) -> Result<DurableAuditReceipt, Diagnostic> {
        let partition = record.partition().to_owned();
        self.with_partition_lock(&partition, || {
            let path = self.partition_path(&partition)?;
            let mut file = OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(path)
                .map_err(|_| unavailable("audit partition cannot be opened for append"))?;
            file.lock()
                .map_err(|_| unavailable("audit partition cannot be locked for append"))?;
            let existing = read_and_verify(&mut file, &partition)?;
            let (sequence, previous_hash) = match existing.last() {
                Some(entry) => (
                    entry
                        .receipt
                        .sequence()
                        .checked_add(1)
                        .ok_or_else(|| unavailable("audit partition sequence exhausted"))?,
                    *entry.receipt.record_hash(),
                ),
                None => (1, Sha256Digest::from_bytes([0; 32])),
            };
            let canonical_record = serde_json_canonicalizer::to_vec(&record)
                .map_err(|_| unavailable("audit record cannot be canonicalized"))?;
            let record_hash = record_hash(&partition, sequence, &previous_hash, &canonical_record);
            let receipt = DurableAuditReceipt::try_new(
                partition.clone(),
                sequence,
                previous_hash,
                record_hash,
            )
            .map_err(unavailable)?;
            let persisted = PersistedEntry {
                partition: partition.clone(),
                sequence,
                previous_hash,
                record_hash,
                record: serde_json::to_value(record)
                    .map_err(|_| unavailable("audit record cannot be serialized"))?,
            };
            file.seek(SeekFrom::End(0))
                .map_err(|_| unavailable("audit partition append position is unavailable"))?;
            let line = serde_json::to_vec(&persisted)
                .map_err(|_| unavailable("audit ledger entry cannot be serialized"))?;
            file.write_all(&line)
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.flush())
                .map_err(|_| unavailable("audit ledger entry cannot be flushed"))?;
            file.sync_data()
                .map_err(|_| unavailable("audit ledger entry cannot be made durable"))?;
            Ok(receipt)
        })
    }
}

#[async_trait]
impl AuditSink for FileAuditLedger {
    async fn append(&self, record: AuditRecord) -> Result<DurableAuditReceipt, Diagnostic> {
        self.append_sync(record)
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedAuditEntry {
    receipt: DurableAuditReceipt,
    record: Value,
}

impl VerifiedAuditEntry {
    pub fn receipt(&self) -> &DurableAuditReceipt {
        &self.receipt
    }

    pub fn record(&self) -> &Value {
        &self.record
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedEntry {
    partition: String,
    sequence: u64,
    previous_hash: Sha256Digest,
    record_hash: Sha256Digest,
    record: Value,
}

fn read_and_verify(
    file: &mut File,
    expected_partition: &str,
) -> Result<Vec<VerifiedAuditEntry>, Diagnostic> {
    let length = file
        .metadata()
        .map_err(|_| unavailable("audit partition metadata cannot be read"))?
        .len();
    if length > 0 {
        file.seek(SeekFrom::End(-1))
            .map_err(|_| unavailable("audit partition tail cannot be read"))?;
        let mut tail = [0_u8; 1];
        file.read_exact(&mut tail)
            .map_err(|_| unavailable("audit partition tail cannot be read"))?;
        if tail != *b"\n" {
            return Err(unavailable("audit ledger has an incomplete final entry"));
        }
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| unavailable("audit partition cannot be read from its beginning"))?;
    let mut verified = Vec::new();
    let mut expected_sequence = 1_u64;
    let mut expected_previous_hash = Sha256Digest::from_bytes([0; 32]);
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|_| unavailable("audit ledger entry cannot be read"))?;
        if line.is_empty() {
            return Err(unavailable("audit ledger contains an empty entry"));
        }
        let persisted: PersistedEntry = serde_json::from_str(&line)
            .map_err(|_| unavailable("audit ledger entry is malformed"))?;
        if persisted.partition != expected_partition
            || persisted.sequence != expected_sequence
            || persisted.previous_hash != expected_previous_hash
        {
            return Err(unavailable(
                "audit ledger partition sequence or previous hash is invalid",
            ));
        }
        let canonical_record = serde_json_canonicalizer::to_vec(&persisted.record)
            .map_err(|_| unavailable("persisted audit record cannot be canonicalized"))?;
        let expected_hash = record_hash(
            expected_partition,
            persisted.sequence,
            &persisted.previous_hash,
            &canonical_record,
        );
        if persisted.record_hash != expected_hash {
            return Err(unavailable("audit ledger record hash verification failed"));
        }
        let receipt = DurableAuditReceipt::try_new(
            persisted.partition,
            persisted.sequence,
            persisted.previous_hash,
            persisted.record_hash,
        )
        .map_err(unavailable)?;
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or_else(|| unavailable("audit partition sequence exhausted"))?;
        expected_previous_hash = *receipt.record_hash();
        verified.push(VerifiedAuditEntry {
            receipt,
            record: persisted.record,
        });
    }
    Ok(verified)
}

fn record_hash(
    partition: &str,
    sequence: u64,
    previous_hash: &Sha256Digest,
    canonical_record: &[u8],
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(AUDIT_HASH_DOMAIN);
    hasher.update(partition.as_bytes());
    hasher.update(sequence.to_be_bytes());
    hasher.update(previous_hash.as_bytes());
    hasher.update(canonical_record);
    Sha256Digest::from_bytes(hasher.finalize().into())
}

fn validate_partition(partition: &str) -> Result<(), Diagnostic> {
    if partition.trim().is_empty()
        || partition.len() > 512
        || partition
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        Err(unavailable("audit partition is invalid"))
    } else {
        Ok(())
    }
}

fn unavailable(message: impl Into<String>) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(
        DiagnosticCode::AuditUnavailable,
        DiagnosticCategory::Audit,
        DiagnosticStage::Audit,
        message.into(),
    );
    diagnostic.retry = RetryClass::AfterRefresh;
    diagnostic
}
