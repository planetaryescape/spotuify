use anyhow::Result;

use crate::output::OutputFormat;
use crate::protocol::DaemonStatus;

pub fn print_status(status: &DaemonStatus, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(status)?),
        OutputFormat::Jsonl => println!("{}", serde_json::to_string(status)?),
        OutputFormat::Csv => {
            println!("running,pid,uptime_secs,socket_path,socket_reachable,stale_socket,version");
            println!(
                "{},{},{},{},{},{},{}",
                status.running,
                status
                    .daemon_pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_default(),
                status
                    .uptime_secs
                    .map(|uptime| uptime.to_string())
                    .unwrap_or_default(),
                csv_value(&status.socket_path),
                status.socket_reachable,
                status.stale_socket,
                status.daemon_version.as_deref().unwrap_or("")
            );
        }
        OutputFormat::Ids => {
            if let Some(pid) = status.daemon_pid {
                println!("{pid}");
            }
        }
        OutputFormat::Table => {
            println!("running\t{}", yes_no(status.running));
            println!(
                "pid\t{}",
                status
                    .daemon_pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
            println!(
                "uptime_secs\t{}",
                status
                    .uptime_secs
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
            println!("socket\t{}", status.socket_path);
            println!("socket_reachable\t{}", yes_no(status.socket_reachable));
            println!("stale_socket\t{}", yes_no(status.stale_socket));
            println!("protocol\t{}", status.protocol_version);
            println!(
                "version\t{}",
                status.daemon_version.as_deref().unwrap_or("-")
            );
        }
    }
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn csv_value(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
