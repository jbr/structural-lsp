mod spike;
mod spike_transport;

use spike::{Options, Server};
use std::path::PathBuf;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("spike") => run_spike(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            print_usage();
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown command `{other}`\n");
            print_usage();
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    eprintln!(
        "structural — agentic LSP toolkit (spike stage)

USAGE:
    structural spike [OPTIONS]

OPTIONS:
    --server <rust|python|both>   which language server(s) to drive (default: both)
    --out <dir>                   output root (default: target/spike)
    --quiesce <secs>              max wait for rust-analyzer quiescence (default: 120)
    --pyright-wait <secs>         bounded wait for pyright, which has no ready signal (default: 5)
    --query-timeout <secs>        per-query timeout (default: 60)
    -h, --help

The spike is read-only: it never applies an edit and never executes a
code-action command."
    );
}

fn run_spike(args: &[String]) -> anyhow::Result<()> {
    let mut servers = vec![Server::Rust, Server::Python];
    let mut out = PathBuf::from("target/spike");
    let mut quiesce = Duration::from_secs(120);
    let mut pyright_wait = Duration::from_secs(5);
    let mut query_timeout = Duration::from_secs(60);

    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args.get(index + 1).map(String::as_str);
        match flag {
            "--server" => {
                let value = value.ok_or_else(|| anyhow::anyhow!("--server needs a value"))?;
                servers = match value {
                    "both" => vec![Server::Rust, Server::Python],
                    other => vec![
                        Server::parse(other)
                            .ok_or_else(|| anyhow::anyhow!("unknown server `{other}`"))?,
                    ],
                };
                index += 2;
            }
            "--out" => {
                out = PathBuf::from(value.ok_or_else(|| anyhow::anyhow!("--out needs a value"))?);
                index += 2;
            }
            "--quiesce" => {
                quiesce = Duration::from_secs(parse_secs(value, "--quiesce")?);
                index += 2;
            }
            "--pyright-wait" => {
                pyright_wait = Duration::from_secs(parse_secs(value, "--pyright-wait")?);
                index += 2;
            }
            "--query-timeout" => {
                query_timeout = Duration::from_secs(parse_secs(value, "--query-timeout")?);
                index += 2;
            }
            other => {
                anyhow::bail!("unknown option `{other}` (try `structural spike --help`)");
            }
        }
    }

    let options = Options {
        servers,
        out,
        quiesce,
        pyright_wait,
        query_timeout,
    };

    let reports = spike::run(&options)?;
    println!();
    for report in &reports {
        println!("=== {} ===", report.server.slug());
        println!(
            "  readiness: {} in {:.1}s (quiescent={})",
            report.readiness.strategy,
            report.readiness.elapsed.as_secs_f32(),
            report.readiness.quiescent
        );
        println!(
            "  queries:   {} total · {} ok · {} error · {} non-empty",
            report.total, report.ok, report.errored, report.nonempty
        );
        println!(
            "  events:    {} notifications/server requests",
            report.events
        );
        println!("  output:    {}", report.dir.display());
        println!(
            "  files:     {} and manifest.json",
            report.dir.join("raw").display()
        );
    }
    Ok(())
}

fn parse_secs(value: Option<&str>, flag: &str) -> anyhow::Result<u64> {
    value
        .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))?
        .parse()
        .map_err(|_| anyhow::anyhow!("{flag} needs a whole number of seconds"))
}
