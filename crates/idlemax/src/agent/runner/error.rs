//! Adapter: the ways starting, signalling or reaping a job can fail.

use std::io;
use std::path::PathBuf;

use idlemax_core::job_run::AdmitError;
use idlemax_core::types::JobId;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("job {0} is already running")]
    AlreadyRunning(JobId),
    #[error("job refused: {0}")]
    Refused(#[from] AdmitError),
    #[error("command is empty")]
    EmptyCommand,
    #[error("job stdin pipe unavailable")]
    StdinUnavailable,
    #[error("write job stdin: {0}")]
    Stdin(#[source] io::Error),
    #[error("program {0:?} not found")]
    ProgramNotFound(String),
    #[error("program {0:?} must be a bare name or an absolute path")]
    RelativeProgram(String),
    #[error("cwd {} is not a directory", .0.display())]
    BadCwd(PathBuf),
    #[error("no home directory for the default cwd")]
    NoHome,
    #[error("{what} {}: {source}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("signal job {id}: {source}")]
    Signal {
        id: JobId,
        #[source]
        source: io::Error,
    },
}
