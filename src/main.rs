//! Process entry point for the standalone C37.118 simulator.

use std::{
    env,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    time::Duration,
};

use c37_118_simulator::{
    identity::RuntimeIdentity,
    server::{ManagementConfig, Server},
    startup::{load_startup, DEFAULT_IMAGE_REF},
    time_health::TimeSynchronizationSource,
};

fn main() {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match parse_command(&arguments) {
        Ok(Command::Healthcheck(management_address)) => {
            if let Err(error) = healthcheck(management_address) {
                eprintln!("c37-118-simulator: {error}");
                std::process::exit(2);
            }
        }
        Ok(Command::Run(arguments)) => {
            if let Err(error) = run(arguments) {
                eprintln!("c37-118-simulator: {error}");
                std::process::exit(2);
            }
        }
        Err(error) => {
            eprintln!("c37-118-simulator: {error}");
            eprintln!(
                "usage: c37-118-simulator healthcheck | run --profile <path> --scenario-catalog <path> --deployment-label <label> --management-bind <address> --time-sync-status-file <path>"
            );
            std::process::exit(2);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Healthcheck(SocketAddr),
    Run(RunArguments),
}

#[derive(Debug, PartialEq, Eq)]
struct RunArguments {
    profile_path: String,
    scenario_catalog_path: String,
    deployment_label: String,
    management_bind: SocketAddr,
    time_sync_status_file: PathBuf,
}

fn parse_command(arguments: &[String]) -> Result<Command, String> {
    if arguments.first().map(String::as_str) == Some("healthcheck") {
        return parse_healthcheck(arguments).map(Command::Healthcheck);
    }
    if arguments.first().map(String::as_str) != Some("run") {
        return Err("expected healthcheck or run command".to_string());
    }

    let mut profile_path = None;
    let mut scenario_catalog_path = None;
    let mut deployment_label = None;
    let mut management_bind = None;
    let mut time_sync_status_file = None;
    let mut index = 1;
    while index < arguments.len() {
        let flag = &arguments[index];
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?
            .clone();
        let slot = match flag.as_str() {
            "--profile" => &mut profile_path,
            "--scenario-catalog" => &mut scenario_catalog_path,
            "--deployment-label" => &mut deployment_label,
            "--management-bind" => &mut management_bind,
            "--time-sync-status-file" => &mut time_sync_status_file,
            _ => return Err(format!("unsupported argument {flag}")),
        };
        if slot.replace(value).is_some() {
            return Err(format!("{flag} may be specified only once"));
        }
        index += 2;
    }

    Ok(Command::Run(RunArguments {
        profile_path: profile_path.ok_or_else(|| "--profile is required".to_string())?,
        scenario_catalog_path: scenario_catalog_path
            .ok_or_else(|| "--scenario-catalog is required".to_string())?,
        deployment_label: deployment_label
            .ok_or_else(|| "--deployment-label is required".to_string())?,
        management_bind: management_bind
            .ok_or_else(|| "--management-bind is required".to_string())?
            .parse()
            .map_err(|error| format!("--management-bind must be a socket address: {error}"))?,
        time_sync_status_file: time_sync_status_file
            .ok_or_else(|| "--time-sync-status-file is required".to_string())?
            .into(),
    }))
}

fn parse_healthcheck(arguments: &[String]) -> Result<SocketAddr, String> {
    match arguments {
        [command, flag, address] if command == "healthcheck" && flag == "--management-address" => {
            address
                .parse()
                .map_err(|error| format!("--management-address must be a socket address: {error}"))
        }
        _ => Err("healthcheck requires --management-address <address>".to_string()),
    }
}

fn run(arguments: RunArguments) -> Result<(), Box<dyn std::error::Error>> {
    let image_ref =
        env::var("C37_118_IMAGE_REF").unwrap_or_else(|_| DEFAULT_IMAGE_REF.to_string());
    let startup = load_startup(
        &arguments.profile_path,
        &arguments.scenario_catalog_path,
        arguments.deployment_label,
        image_ref,
    )?;
    let endpoint_count = startup.profile.endpoints.len();
    let data_rate_hz = startup.profile.endpoints[0].data_rate_hz;
    let scenario_count = startup.scenario_catalog.scenarios().len();
    let deployment_label = startup.deployment_label.clone();
    let runtime_identity = startup.runtime_identity.clone();
    log_startup(
        endpoint_count,
        data_rate_hz,
        scenario_count,
        &deployment_label,
        &runtime_identity,
    )?;
    let mut server = Server::bind_with_management(
        startup.profile,
        startup.scenario_catalog,
        TimeSynchronizationSource::File {
            path: arguments.time_sync_status_file,
        },
        ManagementConfig {
            bind_address: arguments.management_bind,
        },
    )?;
    server.configure_runtime_metadata(deployment_label, runtime_identity);
    Ok(server.run()?)
}

#[derive(serde::Serialize)]
struct StartupLog<'a> {
    event: &'static str,
    message: &'static str,
    pmu_endpoints: usize,
    data_rate_hz: u16,
    scenarios: usize,
    deployment_label: &'a str,
    runtime_identity: &'a RuntimeIdentity,
}

fn log_startup(
    endpoint_count: usize,
    data_rate_hz: u16,
    scenario_count: usize,
    deployment_label: &str,
    runtime_identity: &RuntimeIdentity,
) -> Result<(), serde_json::Error> {
    let record = StartupLog {
        event: "simulator_started",
        message: "starting C37.118 simulator",
        pmu_endpoints: endpoint_count,
        data_rate_hz,
        scenarios: scenario_count,
        deployment_label,
        runtime_identity,
    };
    eprintln!("{}", serde_json::to_string(&record)?);
    Ok(())
}

fn healthcheck(management_address: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&management_address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(b"GET /readyz HTTP/1.1\r\nHost: localhost\r\n\r\n")?;

    let mut status_line = [0_u8; 64];
    let mut received = 0;
    while received < status_line.len() {
        let count = stream.read(&mut status_line[received..])?;
        if count == 0 {
            break;
        }
        received += count;
        if status_line[..received]
            .windows(2)
            .any(|window| window == b"\r\n")
        {
            break;
        }
    }

    if status_line[..received].starts_with(b"HTTP/1.1 200 ") {
        println!("ok");
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::Other,
        "management readiness probe did not return HTTP 200",
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::{parse_command, Command, RunArguments};

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_explicit_immutable_startup_inputs() {
        let command = parse_command(&arguments(&[
            "run",
            "--scenario-catalog",
            "scenarios/baseline.yaml",
            "--deployment-label",
            "wama-lab",
            "--management-bind",
            "0.0.0.0:8080",
            "--time-sync-status-file",
            "/run/c37-118/time-sync-status",
            "--profile",
            "profiles/five-pmu-v2.yaml",
        ]))
        .expect("arguments must parse");

        assert_eq!(
            command,
            Command::Run(RunArguments {
                profile_path: "profiles/five-pmu-v2.yaml".to_string(),
                scenario_catalog_path: "scenarios/baseline.yaml".to_string(),
                deployment_label: "wama-lab".to_string(),
                management_bind: "0.0.0.0:8080".parse().expect("valid test address"),
                time_sync_status_file: "/run/c37-118/time-sync-status".into(),
            })
        );
    }

    #[test]
    fn parses_the_protocol_readiness_probe_address() {
        let command = parse_command(&arguments(&[
            "healthcheck",
            "--management-address",
            "127.0.0.1:8080",
        ]))
        .expect("healthcheck arguments must parse");

        assert_eq!(
            command,
            Command::Healthcheck("127.0.0.1:8080".parse().expect("valid address"))
        );
    }

    #[test]
    fn rejects_a_run_without_a_catalog() {
        let error = parse_command(&arguments(&[
            "run",
            "--profile",
            "profiles/five-pmu-v2.yaml",
            "--deployment-label",
            "wama-lab",
            "--management-bind",
            "0.0.0.0:8080",
            "--time-sync-status-file",
            "/run/c37-118/time-sync-status",
        ]))
        .expect_err("catalog must be required");

        assert_eq!(error, "--scenario-catalog is required");
    }
}
