//! Delivery: TOON list views. Each list shows a few columns by default (AXI minimal
//! schemas), `--fields` adds more, a total says how much is not shown, and an empty list
//! says so in words instead of printing nothing.

use idlemax_core::job_run::inbox::InboxEntry;
use idlemax_core::protocol::{ClusterStatus, DeviceView};
use idlemax_core::quota::QuotaView;
use idlemax_core::schedule::ScheduleView;
use idlemax_core::types::{Event, Job, JobStatus, Recommendation, UnixMs};

use super::failure::CliError;
use super::toon::Node;
use crate::cli::rows::{self, snake, Zone};

/// The columns one list can show, and the ones it shows without `--fields`.
pub struct Catalog {
    name: &'static str,
    all: &'static [&'static str],
    default: &'static [&'static str],
}

type Row = Vec<(&'static str, Node)>;

impl Catalog {
    /// The default columns, then the extra ones asked for; `all` adds every column.
    fn columns(&self, extra: &[String]) -> Result<Vec<&'static str>, CliError> {
        let mut cols = self.default.to_vec();
        for want in extra {
            if want == "all" {
                return Ok(self.all.to_vec());
            }
            let Some(col) = self.all.iter().find(|c| **c == want) else {
                return Err(CliError::usage(
                    format!("unknown field `{want}` for {}", self.name),
                    format!(
                        "valid --fields for {}: {}, all (default: {})",
                        self.name,
                        self.all.join(", "),
                        self.default.join(", ")
                    ),
                ));
            };
            if !cols.contains(col) {
                cols.push(col);
            }
        }
        Ok(cols)
    }

    fn table<T>(
        &self,
        items: &[T],
        extra: &[String],
        row: impl Fn(&T) -> Row,
    ) -> Result<Node, CliError> {
        let cols = self.columns(extra)?;
        let rows = items.iter().map(|item| {
            let cells = row(item);
            Node::Obj(
                cols.iter()
                    .filter_map(|c| cells.iter().find(|(k, _)| k == c))
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            )
        });
        Ok(Node::Arr(rows.collect()))
    }
}

/// The table cells under their column names; "-" (nothing to show) becomes null.
fn named(names: &'static [&'static str], cells: Vec<String>) -> Row {
    names
        .iter()
        .copied()
        .zip(cells)
        .map(|(n, c)| (n, cell(c)))
        .collect()
}

/// A table cell as a value: "-" is null, a whole number stays a number.
fn cell(c: String) -> Node {
    match c.parse::<i64>() {
        _ if c == "-" => Node::Null,
        Ok(n) => Node::Int(n),
        Err(_) => Node::Str(c),
    }
}

fn set(row: &mut Row, name: &str, value: Node) {
    if let Some(slot) = row.iter_mut().find(|(k, _)| *k == name) {
        slot.1 = value;
    }
}

/// `count`, then the list under `key`; or the key alone saying `empty`.
fn list(key: &str, empty: &str, shown: usize, total: Option<u64>, table: Node) -> Node {
    if shown == 0 {
        return Node::obj([(key, Node::str(empty))]);
    }
    let count = match total {
        Some(t) if t > shown as u64 => Node::str(format!("{shown} of {t}")),
        _ => Node::Uint(shown as u64),
    };
    Node::obj([("count", count), (key, table)])
}

const JOB_NAMES: &[&str] = &[
    "id", "name", "status", "device", "cost", "priority", "tries", "age", "result",
];
const JOBS: Catalog = Catalog {
    name: "jobs",
    all: JOB_NAMES,
    default: &["id", "name", "status", "device"],
};

/// Jobs; `total` is how many exist under the same filter, `filter` words the empty state.
pub fn jobs(
    list_of: &[Job],
    total: Option<u64>,
    filter: Option<JobStatus>,
    now: UnixMs,
    extra: &[String],
) -> Result<Node, CliError> {
    let table = JOBS.table(list_of, extra, |j| {
        let mut row = named(JOB_NAMES, rows::job_cells(j, now));
        set(&mut row, "id", Node::Int(j.id));
        set(&mut row, "name", Node::str(&j.spec.name));
        set(&mut row, "priority", Node::Int(i64::from(j.spec.priority)));
        let result = rows::job_result_full(j, now);
        set(
            &mut row,
            "result",
            if result == "-" {
                Node::Null
            } else {
                Node::Str(result)
            },
        );
        row
    })?;
    let empty = match filter {
        Some(s) => format!("0 {s} jobs"),
        None => "0 jobs".to_string(),
    };
    Ok(list("jobs", &empty, list_of.len(), total, table))
}

const DEVICE_NAMES: &[&str] = &[
    "id", "state", "guard", "idle", "cpu", "load", "mem_free", "swap", "disk", "power", "jobs",
];
const DEVICES: Catalog = Catalog {
    name: "devices",
    all: DEVICE_NAMES,
    default: &["id", "state", "guard", "jobs"],
};

pub fn devices(s: &ClusterStatus, now: UnixMs, extra: &[String]) -> Result<Node, CliError> {
    let table = DEVICES.table(&s.devices, extra, |d: &DeviceView| {
        named(DEVICE_NAMES, rows::device_cells(d, now))
    })?;
    let empty = "0 devices have sent a heartbeat yet";
    Ok(list("devices", empty, s.devices.len(), None, table))
}

const QUOTA_NAMES: &[&str] = &[
    "account", "window", "used", "reserve", "headroom", "resets", "spend", "why",
];
const QUOTA: Catalog = Catalog {
    name: "quota",
    all: QUOTA_NAMES,
    default: &[
        "account", "window", "used", "headroom", "resets", "spend", "why",
    ],
};

pub fn quota(
    views: &[QuotaView],
    paused: &[String],
    now: UnixMs,
    extra: &[String],
) -> Result<Node, CliError> {
    let table = QUOTA.table(views, extra, |q| {
        let mut row = named(
            QUOTA_NAMES,
            rows::quota_cells(q, paused.contains(&q.account), now),
        );
        let why = q.reasons.join("; ");
        set(
            &mut row,
            "why",
            if why.is_empty() {
                Node::Null
            } else {
                Node::Str(why)
            },
        );
        row
    })?;
    let empty = "0 AI accounts configured ([[quota.accounts]] in config.toml)";
    Ok(list("quota", empty, views.len(), None, table))
}

const ADVICE_NAMES: &[&str] = &[
    "id",
    "kind",
    "title",
    "confidence",
    "source",
    "status",
    "age",
    "why",
];
const ADVICE: Catalog = Catalog {
    name: "advice",
    all: ADVICE_NAMES,
    default: &["id", "kind", "title", "status"],
};

pub fn advice(recs: &[Recommendation], now: UnixMs, extra: &[String]) -> Result<Node, CliError> {
    let table = ADVICE.table(recs, extra, |r| {
        vec![
            ("id", Node::Int(r.id)),
            ("kind", Node::str(snake(&r.kind))),
            ("title", Node::str(&r.title)),
            ("confidence", Node::Float(r.confidence)),
            ("source", Node::str(snake(&r.source))),
            ("status", Node::str(snake(&r.status))),
            ("age", Node::str(rows::rel(r.at_ms, now))),
            ("why", Node::str(&r.why)),
        ]
    })?;
    Ok(list("advice", "0 recommendations", recs.len(), None, table))
}

const EVENT_NAMES: &[&str] = &["time", "kind", "device", "job", "message"];
const EVENTS: Catalog = Catalog {
    name: "events",
    all: EVENT_NAMES,
    default: EVENT_NAMES,
};

pub fn events(list_of: &[Event], zone: Zone, extra: &[String]) -> Result<Node, CliError> {
    let table = EVENTS.table(list_of, extra, |e| {
        vec![
            ("time", Node::str(iso(e.at_ms, zone))),
            ("kind", Node::str(snake(&e.kind))),
            ("device", e.device.as_deref().map_or(Node::Null, Node::str)),
            ("job", e.job.map_or(Node::Null, Node::Int)),
            ("message", Node::str(&e.message)),
        ]
    })?;
    Ok(list(
        "events",
        "0 events recorded yet",
        list_of.len(),
        None,
        table,
    ))
}

/// RFC 3339 time in `zone`: "2026-11-08T13:39:22+06:00" locally, "2026-11-08T07:39:22Z" in UTC.
fn iso(at_ms: UnixMs, zone: Zone) -> String {
    use chrono::SecondsFormat::Secs;
    chrono::DateTime::from_timestamp_millis(at_ms).map_or_else(
        || "-".into(),
        |t| match zone {
            Zone::Local => t.with_timezone(&chrono::Local).to_rfc3339_opts(Secs, false),
            Zone::Utc => t.to_rfc3339_opts(Secs, true),
        },
    )
}

const SCHEDULE_NAMES: &[&str] = &["id", "name", "every", "state", "last_result"];
const SCHEDULES: Catalog = Catalog {
    name: "schedules",
    all: SCHEDULE_NAMES,
    default: SCHEDULE_NAMES,
};

pub fn schedules(views: &[ScheduleView], now: UnixMs, extra: &[String]) -> Result<Node, CliError> {
    let table = SCHEDULES.table(views, extra, |v| {
        let mut row = named(SCHEDULE_NAMES, rows::schedule_cells(v, now));
        set(&mut row, "id", Node::Int(v.schedule.id));
        set(&mut row, "name", Node::str(&v.schedule.spec.name));
        row
    })?;
    Ok(list("schedules", "0 schedules", views.len(), None, table))
}

const INBOX_NAMES: &[&str] = &["time", "job", "name", "result"];
const INBOX: Catalog = Catalog {
    name: "inbox",
    all: INBOX_NAMES,
    default: INBOX_NAMES,
};

pub fn inbox(entries: &[InboxEntry], extra: &[String]) -> Result<Node, CliError> {
    let table = INBOX.table(entries, extra, |e| {
        vec![
            ("time", Node::str(&e.time)),
            ("job", Node::Int(e.job_id)),
            ("name", Node::str(&e.name)),
            ("result", Node::str(&e.result)),
        ]
    })?;
    Ok(list("inbox", "0 notes", entries.len(), None, table))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::out::toon::encode;

    fn job(id: i64, name: &str, status: JobStatus, error: Option<&str>) -> Job {
        let mut j = Job {
            id,
            status,
            last_error: error.map(str::to_string),
            ..Job::default()
        };
        j.spec.name = name.into();
        j
    }

    #[test]
    fn jobs_show_four_columns_a_total_and_widen_on_request() {
        let jobs_in = [
            job(7, "index", JobStatus::Queued, None),
            job(8, "sweep", JobStatus::Failed, Some("exit 2: disk full")),
        ];
        let view = |extra: &[&str]| {
            let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
            encode(&jobs(&jobs_in, Some(143), None, 0, &extra).unwrap())
        };
        assert_eq!(
            view(&[]),
            "count: 2 of 143\njobs[2]{id,name,status,device}:\n  7,index,queued,null\n  8,sweep,failed,null"
        );
        assert_eq!(
            view(&["result"]),
            "count: 2 of 143\njobs[2]{id,name,status,device,result}:\n  \
             7,index,queued,null,null\n  8,sweep,failed,null,\"exit 2: disk full\""
        );
    }

    #[test]
    fn event_times_follow_the_zone() {
        assert_eq!(iso(86_400_000, Zone::Utc), "1970-01-02T00:00:00Z");
        let e = Event {
            at_ms: 3_600_000,
            message: "up".into(),
            ..Event::default()
        };
        let table = encode(&events(&[e], Zone::Utc, &[]).unwrap());
        assert!(table.contains("\"1970-01-01T01:00:00Z\","), "{table}");
    }

    #[test]
    fn an_empty_list_says_what_is_empty_and_a_bad_field_says_what_is_valid() {
        assert_eq!(
            encode(&jobs(&[], Some(0), Some(JobStatus::Queued), 0, &[]).unwrap()),
            "jobs: 0 queued jobs"
        );
        assert_eq!(
            encode(&jobs(&[], None, None, 0, &[]).unwrap()),
            "jobs: 0 jobs"
        );
        let err = jobs(&[], None, None, 0, &["bogus".to_string()]).unwrap_err();
        assert_eq!(err.code, super::super::failure::USAGE);
        assert!(err.what.contains("`bogus`"), "{}", err.what);
        assert!(err.help.unwrap().contains("id, name, status"));
    }
}
