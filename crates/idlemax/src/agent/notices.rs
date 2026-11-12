//! Adapter: delivers the notices of a heartbeat reply on this device (D17, A22). Each becomes
//! one inbox note, the reply one desktop notification when `agent.notify` is true, and the
//! newest id goes to the cursor file, which the next heartbeat sends as `notices_after`.

use std::path::{Path, PathBuf};

use idlemax_core::job_run::inbox::{notice_file_name, render_notice};
use idlemax_core::notify::notice::Notice;
use idlemax_core::notify::Notification;
use idlemax_core::Paths;
use tracing::warn;

use super::runner::write_private;

const CURSOR: &str = "notices.cursor";

fn cursor_file(paths: &Paths) -> PathBuf {
    paths.agent_state_dir().join(CURSOR)
}

/// The id of the newest delivered notice, 0 when none was. A few bytes, read per heartbeat.
pub fn cursor(paths: &Paths) -> i64 {
    std::fs::read_to_string(cursor_file(paths))
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0)
}

/// Writes a note per notice, shows one notification for the whole reply when `notify` is
/// true, then moves the cursor. A note is named by the notice id, so a notice delivered
/// again after a crash replaces its note; only the notification can repeat once.
pub fn deliver(paths: &Paths, notices: &[Notice], notify: bool, show: &dyn Fn(&Notification)) {
    let Some(newest) = notices.iter().map(|n| n.id).max() else {
        return;
    };
    let offset = chrono::Local::now().offset().local_minus_utc();
    let dir = paths.inbox_dir();
    for n in notices {
        let name = notice_file_name(n, offset);
        if let Err(e) = write_atomic(&dir, &name, &render_notice(n, offset)) {
            warn!(notice = n.id, error = %e, "cannot write the notice's inbox note");
        }
    }
    if notify {
        show(&notification(notices));
    }
    let at = newest.max(cursor(paths));
    if let Err(e) = write_atomic(&paths.agent_state_dir(), CURSOR, &at.to_string()) {
        warn!(error = %e, "cannot write the notice cursor");
    }
}

fn notification(notices: &[Notice]) -> Notification {
    match notices {
        [one] => Notification {
            title: one.title.clone(),
            message: one.body.clone(),
        },
        many => Notification {
            title: format!("idlemax: {} notices", many.len()),
            message: "see idlemax inbox".into(),
        },
    }
}

fn write_atomic(dir: &Path, name: &str, text: &str) -> std::io::Result<()> {
    let (tmp, path) = (dir.join(format!(".{name}.tmp")), dir.join(name));
    crate::state::create_private_dir(dir)?;
    write_private(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use idlemax_core::notify::notice::NoticeKind;

    use super::*;

    fn notice(id: i64) -> Notice {
        Notice {
            id,
            at_ms: 1_800_000_000_000,
            kind: NoticeKind::TaskFinished,
            title: format!("idlemax: task {id}"),
            body: "checks passed".into(),
        }
    }

    #[test]
    fn a_notice_becomes_one_note_and_one_notification_per_reply() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let shown: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let show = |n: &Notification| shown.borrow_mut().push(n.title.clone());
        let notes = || std::fs::read_dir(paths.inbox_dir()).unwrap().count();

        deliver(&paths, &[notice(1)], false, &show);
        deliver(&paths, &[notice(1)], false, &show);
        assert_eq!(notes(), 1);
        assert_eq!(cursor(&paths), 1);
        assert!(shown.borrow().is_empty());

        deliver(&paths, &[notice(2)], true, &show);
        deliver(&paths, &[notice(3), notice(4), notice(5)], true, &show);
        assert_eq!(*shown.borrow(), ["idlemax: task 2", "idlemax: 3 notices"]);
        assert_eq!(cursor(&paths), 5);
        assert_eq!(notes(), 5);
    }
}
