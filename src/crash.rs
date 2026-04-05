use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const CRASH_AT_ENV: &str = "GIT_RELAY_CRASH_AT";
const CHECKPOINT_LOG_ENV: &str = "GIT_RELAY_CHECKPOINT_LOG";
const CRASH_EXIT_CODE: i32 = 97;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashCheckpoint {
    BeforePreReceive,
    AfterPreReceiveSuccess,
    AfterReferenceTransactionPrepared,
    AfterReferenceTransactionCommitted,
    AfterReceivePackSuccessBeforeWrapperExit,
    AfterWrapperFlushesResponse,
}

impl CrashCheckpoint {
    pub fn as_str(self) -> &'static str {
        match self {
            CrashCheckpoint::BeforePreReceive => "before_pre_receive",
            CrashCheckpoint::AfterPreReceiveSuccess => "after_pre_receive_success",
            CrashCheckpoint::AfterReferenceTransactionPrepared => {
                "after_reference_transaction_prepared"
            }
            CrashCheckpoint::AfterReferenceTransactionCommitted => {
                "after_reference_transaction_committed"
            }
            CrashCheckpoint::AfterReceivePackSuccessBeforeWrapperExit => {
                "after_receive_pack_success_before_wrapper_exit"
            }
            CrashCheckpoint::AfterWrapperFlushesResponse => "after_wrapper_flushes_response",
        }
    }
}

pub fn checkpointing_enabled() -> bool {
    std::env::var_os(CRASH_AT_ENV).is_some() || std::env::var_os(CHECKPOINT_LOG_ENV).is_some()
}

pub fn hit_checkpoint(checkpoint: CrashCheckpoint) {
    record_checkpoint(checkpoint);
    if crash_requested(checkpoint) {
        crash_process();
    }
}

fn record_checkpoint(checkpoint: CrashCheckpoint) {
    let Some(path) = std::env::var_os(CHECKPOINT_LOG_ENV) else {
        return;
    };
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{}", checkpoint.as_str());
}

fn crash_requested(checkpoint: CrashCheckpoint) -> bool {
    let Ok(value) = std::env::var(CRASH_AT_ENV) else {
        return false;
    };
    value
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .map(str::trim)
        .any(|candidate| !candidate.is_empty() && candidate == checkpoint.as_str())
}

fn crash_process() -> ! {
    #[cfg(unix)]
    unsafe {
        libc::_exit(CRASH_EXIT_CODE);
    }

    #[cfg(not(unix))]
    std::process::exit(CRASH_EXIT_CODE);
}
