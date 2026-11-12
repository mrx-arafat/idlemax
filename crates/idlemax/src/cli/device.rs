//! Delivery: `idlemax device list|on|off`, the owner's switch per device for dispatcher work.
//! Every device starts off (D19, A23); plain jobs never depend on the switch.

use anyhow::Result;
use clap::Subcommand;
use idlemax_core::control::device::SwitchReply;
use idlemax_core::protocol::DeviceView;

use super::out::toon::Node;
use super::print::table;
use super::{noop, ApiClient, Out};

#[derive(Subcommand, Debug)]
pub enum DeviceCmd {
    /// List the devices and whether the dispatcher may use each one
    List,
    /// Let the dispatcher use a device; every device starts off
    On { id: String },
    /// Stop the dispatcher's work on a device; plain jobs keep running
    Off { id: String },
    /// Make the device's own credential, which opens only the agent routes; shown once
    Credential {
        id: String,
        /// Print the credential alone, for `| idlemax setup --role agent --token-stdin`
        #[arg(long, conflicts_with = "revoke")]
        raw: bool,
        /// Remove the device's credential; its agent is refused from then on
        #[arg(long)]
        revoke: bool,
    },
}

pub async fn run(cmd: DeviceCmd, c: &ApiClient, out: &Out) -> Result<()> {
    let (id, on) = match cmd {
        DeviceCmd::List => return list(c, out).await,
        DeviceCmd::Credential { id, raw, revoke } => {
            return credential(c, &id, raw, revoke, out).await
        }
        DeviceCmd::On { id } => (id, true),
        DeviceCmd::Off { id } => (id, false),
    };
    let reply = c.switch_device(&id, on).await?;
    let text = result_text(&reply);
    out.emit_with(
        &reply,
        |_| format!("device {}: {text}", reply.device),
        || Ok(switch_node(&text, &reply)),
        &switch_hints(&reply),
    )
}

/// Prints a new credential (alone with `--raw`), or revokes the device's credential.
async fn credential(c: &ApiClient, id: &str, raw: bool, revoke: bool, out: &Out) -> Result<()> {
    if revoke {
        let reply = c.revoke_device_credential(id).await?;
        let text = if reply.removed {
            "credential revoked".to_string()
        } else {
            noop::already("without a credential")
        };
        return out.emit_with(
            &reply,
            |_| format!("device {}: {text}", reply.device),
            || {
                Ok(Node::obj([
                    ("result", Node::str(&text)),
                    ("device", Node::str(&reply.device)),
                ]))
            },
            &[],
        );
    }
    let reply = c.device_credential(id).await?;
    if raw {
        println!("{}", reply.credential);
        return Ok(());
    }
    let next = format!(
        "idlemax device credential {id} --raw | ssh <server> idlemax setup --role agent \
         --coord-url <url> --token-stdin"
    );
    out.emit_with(
        &reply,
        |r| {
            format!(
                "device {}: credential {} (shown once; it replaces any older one)",
                r.device, r.credential
            )
        },
        || {
            Ok(Node::obj([
                ("device", Node::str(&reply.device)),
                ("credential", Node::str(&reply.credential)),
            ]))
        },
        &[hint(
            &next,
            "give the server its credential in place of the owner token",
        )],
    )
}

async fn list(c: &ApiClient, out: &Out) -> Result<()> {
    let devices = c.status().await?.devices;
    let hints = if devices.iter().any(|d| d.enabled != Some(true)) {
        vec![hint(
            "idlemax device on <id>",
            "let the dispatcher use a device",
        )]
    } else {
        Vec::new()
    };
    out.emit_with(
        &devices,
        |d| device_table(d),
        || Ok(device_list(&devices)),
        &hints,
    )
}

fn hint(cmd: &str, to: &str) -> String {
    format!("Run `{cmd}` to {to}")
}

fn on_off(on: Option<bool>) -> &'static str {
    match on {
        Some(true) => "on",
        Some(false) => "off",
        None => "unknown",
    }
}

/// What the switch did, or the no-op.
fn result_text(r: &SwitchReply) -> String {
    let state = on_off(Some(r.enabled));
    if !r.changed {
        return noop::already(state);
    }
    if r.enabled {
        return "switched on".into();
    }
    format!(
        "switched off: {} worker(s) and {} dispatcher job(s) stopped",
        r.runs_stopped, r.jobs_stopped
    )
}

fn switch_node(result: &str, r: &SwitchReply) -> Node {
    Node::obj([
        ("result", Node::str(result)),
        ("device", Node::str(&r.device)),
        ("enabled", Node::Bool(r.enabled)),
        ("changed", Node::Bool(r.changed)),
        ("runs_stopped", Node::Uint(u64::from(r.runs_stopped))),
        ("jobs_stopped", Node::Uint(u64::from(r.jobs_stopped))),
    ])
}

fn switch_hints(r: &SwitchReply) -> Vec<String> {
    if r.enabled {
        let cmd = format!("idlemax project list --device {}", r.device);
        vec![hint(&cmd, "see which of its projects are on")]
    } else {
        let cmd = format!("idlemax device on {}", r.device);
        vec![hint(&cmd, "let the dispatcher continue its workers there")]
    }
}

fn device_table(all: &[DeviceView]) -> String {
    if all.is_empty() {
        return "no devices yet: start `idlemax agent` on one".into();
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|d| {
            let room = d
                .room
                .as_ref()
                .map_or("-".to_string(), |r| r.state.as_str().to_string());
            let online = if d.online { "online" } else { "offline" };
            vec![
                d.info.id.clone(),
                on_off(d.enabled).into(),
                online.into(),
                room,
            ]
        })
        .collect();
    table(&["DEVICE", "DISPATCHER", "SEEN", "ROOM"], &rows)
}

fn device_list(all: &[DeviceView]) -> Node {
    let rows = all
        .iter()
        .map(|d| {
            Node::obj([
                ("id", Node::str(&d.info.id)),
                ("enabled", d.enabled.map_or(Node::Null, Node::Bool)),
                ("online", Node::Bool(d.online)),
                (
                    "room",
                    d.room
                        .as_ref()
                        .map_or(Node::Null, |r| Node::str(r.state.as_str())),
                ),
            ])
        })
        .collect();
    Node::obj([("devices", Node::Arr(rows))])
}
