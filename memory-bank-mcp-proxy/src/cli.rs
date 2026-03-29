use clap::Parser;

#[derive(Debug, Parser)]
#[command(author, version, about = "Memory Bank stdio MCP proxy")]
pub struct ProxyArgs {
    #[arg(long, env = "MEMORY_BANK_SERVER_URL")]
    pub server_url: Option<String>,
}

impl ProxyArgs {
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }
}

#[cfg(test)]
mod tests {
    use super::ProxyArgs;
    use clap::Parser;

    #[test]
    fn parse_defaults_server_url() {
        let args = ProxyArgs::try_parse_from(["memory-bank-mcp-proxy"]).expect("parse");
        assert_eq!(args.server_url, None);
    }

    #[test]
    fn parse_accepts_explicit_server_url() {
        let args = ProxyArgs::try_parse_from([
            "memory-bank-mcp-proxy",
            "--server-url",
            "http://127.0.0.1:9090/",
        ])
        .expect("parse");
        assert_eq!(args.server_url.as_deref(), Some("http://127.0.0.1:9090/"));
    }
}
