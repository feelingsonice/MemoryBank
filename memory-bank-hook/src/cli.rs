use clap::Parser;

#[derive(Debug, Parser)]
#[command(author, version, about = "Memory Bank hook client")]
pub struct HookArgs {
    #[arg(long)]
    pub agent: String,

    #[arg(long)]
    pub event: String,

    #[arg(long, default_value = "http://127.0.0.1:8080")]
    pub server_url: String,
}

impl HookArgs {
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }
}

#[cfg(test)]
mod tests {
    use super::HookArgs;
    use clap::Parser;

    #[test]
    fn hook_parses_required_fields() {
        let args = HookArgs::try_parse_from([
            "memory-bank-hook",
            "--agent",
            "claude-code",
            "--event",
            "Stop",
        ])
        .expect("parse hook");

        assert_eq!(args.agent, "claude-code");
        assert_eq!(args.event, "Stop");
        assert_eq!(args.server_url, "http://127.0.0.1:8080");
    }

    #[test]
    fn hook_accepts_explicit_server_url() {
        let args = HookArgs::try_parse_from([
            "memory-bank-hook",
            "--agent",
            "claude-code",
            "--event",
            "Stop",
            "--server-url",
            "http://127.0.0.1:9090/",
        ])
        .expect("parse hook");

        assert_eq!(args.server_url, "http://127.0.0.1:9090/");
    }

    #[test]
    fn hook_rejects_namespace_flag() {
        assert!(
            HookArgs::try_parse_from([
                "memory-bank-hook",
                "--namespace",
                "default",
                "--agent",
                "claude-code",
                "--event",
                "Stop",
            ])
            .is_err()
        );
    }

    #[test]
    fn hook_args_keeps_inputs() {
        let config = HookArgs {
            agent: "windsurf".to_string(),
            event: "AfterAgent".to_string(),
            server_url: "http://127.0.0.1:8080".to_string(),
        };

        assert_eq!(config.agent, "windsurf");
        assert_eq!(config.event, "AfterAgent");
        assert_eq!(config.server_url, "http://127.0.0.1:8080");
    }
}
