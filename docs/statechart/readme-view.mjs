// The README view of the idlemax job lifecycle: as few boxes and arrows as tell the story.
// State names are the statuses `idlemax jobs` prints.
// Hidden on purpose, and said in words in the README instead:
// - cancel steps (you can cancel at any time; the job ends as cancelled),
// - a device dropping the job while it is starting or paused (it goes the same way as from Running).

const place = {
  queued: "Waiting (queued)",
  leased: "Starting on a device (leased)",
  running: "Running",
  paused: "Paused",
  succeeded: "Succeeded",
  failed: "Failed, killed or cancelled",
  killed: "Failed, killed or cancelled",
  cancelled: "Failed, killed or cancelled",
  parked: "Needs you (parked)",
};

const phase = {
  queued: "In progress",
  leased: "In progress",
  running: "In progress",
  paused: "On hold",
  succeeded: "Succeeded",
  failed: "Stopped",
  killed: "Stopped",
  cancelled: "Stopped",
  parked: "Needs you",
};

export const state = (v) => place[v.status];
export const group = (v) => phase[v.status];
export const colors = {
  "In progress": "blue",
  "On hold": "yellow",
  Succeeded: "teal",
  Stopped: "orange",
  "Needs you": "pink",
};

export const step = (action, from, to) => {
  switch (action) {
    case "Claim":
      return "a device has room";
    case "Start":
      return "it starts";
    case "Busy":
      return "the machine gets busy";
    case "Resume":
      return "the machine is free again";
    case "Succeed":
      return "exit 0";
    case "Fail":
    case "Kill":
      return "it fails or hits a limit";
    case "LoseLease":
      if (from.status !== "running") return null;
      return to.status === "parked" ? "dropped too many times" : "the device drops it";
    case "ReturnUnstarted":
    case "Cancel":
    case "Idle":
    case "Done":
      return null;
    default:
      throw new Error(`unknown action ${action}`);
  }
};
