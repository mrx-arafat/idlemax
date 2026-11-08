//! Delivery: how a failed command is printed. A cause that its parent already quotes is not
//! repeated, because most of our errors put `{source}` in their message and also expose it.

pub fn render(err: &anyhow::Error) -> String {
    let mut out = String::new();
    for cause in err.chain() {
        let text = cause.to_string();
        let text = text.trim();
        if text.is_empty() || out.contains(text) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(": ");
        }
        out.push_str(text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::state::StateError;
    use idlemax_core::Config;

    #[test]
    fn a_bad_config_value_is_reported_once_with_its_key() {
        let source = Config::parse("[guard]\nswap_growth_kill = 100000000000\n").unwrap_err();
        let path = "/home/me/.idlemax/config.toml".into();
        let shown = render(&StateError::Config { path, source }.into());
        assert_eq!(shown.matches("swap_growth_kill").count(), 1, "{shown:?}");
        assert_eq!(shown.lines().count(), 1, "{shown:?}");
        assert!(
            shown.starts_with("/home/me/.idlemax/config.toml: "),
            "{shown:?}"
        );
    }

    #[test]
    fn a_cause_the_message_does_not_quote_is_still_shown() {
        let outer = anyhow::anyhow!("gone").context("reading the token");
        assert_eq!(render(&outer), "reading the token: gone");
    }
}
