use std::time::{Duration, Instant};

/// A service endpoint to wait for.
pub struct ServiceEndpoint {
    pub name: String,
    pub url: String,
}

impl ServiceEndpoint {
    /// Create a Qdrant service endpoint.
    pub fn qdrant(host: &str, port: u16) -> Self {
        Self {
            name: format!("Qdrant at {host}:{port}"),
            url: format!("http://{host}:{port}/readyz"),
        }
    }

    /// Create an Elasticsearch service endpoint.
    pub fn elasticsearch(host: &str, port: u16) -> Self {
        Self {
            name: format!("Elasticsearch at {host}:{port}"),
            url: format!("http://{host}:{port}/_cluster/health"),
        }
    }
}

/// Poll a single service endpoint until it responds with 200.
pub fn wait_for_service(
    endpoint: &ServiceEndpoint,
    timeout: Duration,
    interval: Duration,
) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    eprintln!("Waiting for {}...", endpoint.name);
    let start = Instant::now();

    loop {
        match client.get(&endpoint.url).send() {
            Ok(resp) if resp.status().is_success() => {
                eprintln!("{} is ready", endpoint.name);
                return Ok(());
            }
            _ => {}
        }

        if start.elapsed() >= timeout {
            return Err(format!(
                "{} did not become ready within {:.0}s",
                endpoint.name,
                timeout.as_secs_f64()
            ));
        }
        std::thread::sleep(interval);
    }
}

/// Wait for all services required by a profile.
///
/// Profiles: "qdrant", "elastic", "mixed", "all"
pub fn wait_for_services(profile: &str) -> Result<(), String> {
    let timeout = Duration::from_secs(
        std::env::var("TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120),
    );
    let interval = Duration::from_secs(
        std::env::var("INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5),
    );

    let endpoints: Vec<ServiceEndpoint> = match profile {
        "qdrant" => vec![
            ServiceEndpoint::qdrant("localhost", 6333),
            ServiceEndpoint::qdrant("localhost", 6334),
        ],
        "elastic" => vec![
            ServiceEndpoint::elasticsearch("localhost", 9200),
            ServiceEndpoint::elasticsearch("localhost", 9201),
        ],
        "mixed" => vec![
            ServiceEndpoint::qdrant("localhost", 6333),
            ServiceEndpoint::elasticsearch("localhost", 9200),
        ],
        "all" => vec![
            ServiceEndpoint::qdrant("localhost", 6333),
            ServiceEndpoint::qdrant("localhost", 6335),
            ServiceEndpoint::elasticsearch("localhost", 9200),
            ServiceEndpoint::elasticsearch("localhost", 9201),
        ],
        _ => {
            return Err(format!(
                "Unknown profile: {profile}. Use: qdrant, elastic, mixed, all"
            ))
        }
    };

    for ep in &endpoints {
        wait_for_service(ep, timeout, interval)?;
    }

    eprintln!("All services are ready!");
    Ok(())
}
