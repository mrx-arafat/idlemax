//! Domain: the OpenCode plugin that injects the idlemax summary into the system prompt once
//! per session. Pure: the plugin source as text, and how to recognise and read it back.
//! The hook name and `output.system` come from OpenCode's plugin API as the axi-sdk-js
//! ambient-context plugin uses them.

use serde_json::Value;

use super::hooks::SUBCOMMAND;

/// File name under `~/.config/opencode/plugins/`.
pub const FILE_NAME: &str = "idlemax.js";
const MARKER: &str = "idlemax managed opencode plugin";
const PROGRAM_LINE: &str = "const program = ";

/// True for a file idlemax wrote; anything else is the user's and is never overwritten.
pub fn is_managed(text: &str) -> bool {
    text.lines().next().is_some_and(|l| l.contains(MARKER))
}

/// The program the plugin runs, read back from a managed file.
pub fn program_of(text: &str) -> Option<String> {
    let line = text.lines().find_map(|l| l.strip_prefix(PROGRAM_LINE))?;
    serde_json::from_str(line.trim_end_matches(';')).ok()
}

/// The plugin source for `program` (a bare name on PATH or an absolute path), with the state
/// folder as `--home` when it is not the default.
pub fn plugin_source(program: &str, home: Option<&str>) -> String {
    let mut args: Vec<&str> = SUBCOMMAND.split(' ').collect();
    args.extend(
        home.map(|dir| [super::hooks::HOME_FLAG, dir])
            .into_iter()
            .flatten(),
    );
    let (program, args) = (Value::from(program), Value::from(args));
    format!(
        r###"// {MARKER}: written by `idlemax hooks install`; delete this line to keep your own edits.
import {{ spawn }} from "node:child_process";

{PROGRAM_LINE}{program};
const args = {args};
const timeoutMs = 10000;

// The summary, or "" on any failure: a broken hook must never reach the model's context.
function summary(cwd) {{
  return new Promise((resolve) => {{
    let out = "";
    let done = false;
    const finish = (text) => {{
      if (done) return;
      done = true;
      clearTimeout(timer);
      resolve(text.trim());
    }};
    const child = spawn(program, args, {{
      cwd: cwd || process.cwd(),
      env: process.env,
      shell: false,
      stdio: ["ignore", "pipe", "ignore"],
    }});
    const timer = setTimeout(() => {{
      child.kill("SIGTERM");
      finish("");
    }}, timeoutMs);
    child.stdout.setEncoding("utf-8");
    child.stdout.on("data", (chunk) => {{
      out += chunk;
    }});
    child.on("error", () => finish(""));
    child.on("close", () => finish(out));
  }});
}}

export const IdlemaxSessionSummaryPlugin = async ({{ directory }}) => {{
  const bySession = new Map();
  return {{
    "experimental.chat.system.transform": async (input, output) => {{
      const id = input.sessionID ?? "__global__";
      if (!bySession.has(id)) bySession.set(id, await summary(directory));
      const text = bySession.get(id);
      if (text) output.system.push("## idlemax\n" + text);
    }},
  }};
}};
"###
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_is_recognised_and_its_program_reads_back_even_when_awkward() {
        for program in [
            "idlemax",
            "/Users/me/my bin/idlemax",
            "/tmp/we\"ird\\path/idlemax",
            "/tmp/ünï/idlemax",
        ] {
            let src = plugin_source(program, None);
            assert!(is_managed(&src));
            assert_eq!(program_of(&src).as_deref(), Some(program), "{src}");
            assert!(
                src.contains(r#"const args = ["hooks","summary"];"#),
                "{src}"
            );
        }
        let custom = plugin_source("idlemax", Some("/srv/state"));
        assert!(
            custom.contains(r#"const args = ["hooks","summary","--home","/srv/state"];"#),
            "{custom}"
        );
        assert!(!is_managed("export const Mine = async () => ({});\n"));
        assert!(!is_managed(
            "import x from 'y';\n// idlemax managed opencode plugin\n"
        ));
        assert_eq!(program_of("no program here"), None);
    }
}
