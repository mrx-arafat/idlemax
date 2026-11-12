//! Domain: the `config.toml` of the sandbox's Codex home (review finding 5): the owner's file
//! without `mcp_servers`, so a sandboxed Codex worker never starts the owner's MCP servers. Pure.

/// The owner's Codex config without its MCP servers; an unreadable file gives an empty one.
pub fn sandbox_codex_config(owner: &str) -> String {
    let Ok(mut table) = owner.parse::<toml::Table>() else {
        return String::new();
    };
    table.remove("mcp_servers");
    toml::to_string(&table).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owners_mcp_servers_stay_out_and_the_rest_stays() {
        let owner = "model = \"gpt-5\"\n[mcp_servers.db]\ncommand = \"psql\"\n[tui]\nx = 1\n";
        let out = sandbox_codex_config(owner);
        assert!(out.contains("model = \"gpt-5\""), "{out}");
        assert!(out.contains("[tui]"), "{out}");
        assert!(
            !out.contains("mcp_servers") && !out.contains("psql"),
            "{out}"
        );
        assert_eq!(sandbox_codex_config("not = = toml"), "");
    }
}
