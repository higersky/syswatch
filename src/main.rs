mod metrics;
mod nvml_metrics;
mod utils;

use actix_web::http::{StatusCode, Uri};
use anyhow::{Context, Result};
use clap::Parser;
// use env_logger::Env;
use reqwest::{Client, Url};

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use actix_web::{get, web, App, HttpResponse, HttpServer};
use prometheus_client::encoding::text::encode;

use prometheus_client::registry::Registry;
use std::net::SocketAddr;

use crate::metrics::KeepAliveConfig;
use crate::nvml_metrics::NvmlMetricsCollector;
use crate::utils::IntoHttpError;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Service address
    #[arg(short, long, default_value = "0.0.0.0")]
    address: String,

    /// Service port
    #[arg(short, long, default_value = "9101")]
    port: u16,

    /// Show all users
    #[arg(short, long)]
    show_all_users: bool,

    /// Combine results with upstream
    #[arg(short, long)]
    combine_with_upstream: bool,

    /// Upstream port
    #[arg(short, long, default_value = "9100")]
    upstream_port: u16,

    /// Keep alive check service
    #[arg(long)]
    alive_check: bool,

    #[arg(long, default_value = "/etc/syswatch.toml")]
    alive_check_config: PathBuf,
}

struct AppState {
    registry: Registry,
    collector: NvmlMetricsCollector,
}

struct AppReadOnlyConfig {
    upstream: bool,
    upstream_port: u16,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // tracing_subscriber::fmt::init();
    // env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let addr: SocketAddr = format!("{}:{}", args.address, args.port)
        .parse()
        .with_context(|| "Cannot parse listen address")?;

    let collector = NvmlMetricsCollector::new(args.show_all_users)?;
    let metrics = web::Data::new(metrics::Metrics::new());
    let alive_status = web::Data::new(metrics::AliveStatus::default());

    let registry = build_registry(&metrics, &alive_status);

    let keep_alive_config = if args.alive_check {
        let keep_alive_config = std::fs::read_to_string(&args.alive_check_config)?;
        let keep_alive_config: KeepAliveConfig = toml::from_str(&keep_alive_config)?;
        if keep_alive_config.interval == 0 {
            anyhow::bail!("Keep alive configuration error: interval should be larger than 0");
        }
        if keep_alive_config.item.is_empty() {
            anyhow::bail!("Keep alive configuration error: no item found");
        }
        println!(
            "Alive check is enabled. Interval = {} s\nMachine List:",
            keep_alive_config.interval
        );
        for item in keep_alive_config.item.iter() {
            let _uri: Uri = Uri::from_str(item.url.as_str()).with_context(|| {
                format!(
                    "Parsing alive check config {}",
                    args.alive_check_config.to_string_lossy()
                )
            })?;
            println!("- {}: {}", item.hostname, item.url);
        }
        Some(keep_alive_config)
    } else {
        None
    };

    let state = AppState {
        registry,
        collector,
    };

    let state = web::Data::new(Mutex::new(state));

    let config = AppReadOnlyConfig {
        upstream: args.combine_with_upstream,
        upstream_port: args.upstream_port,
    };

    if args.combine_with_upstream {
        println!("Upstream port is set as {}", args.upstream_port);
    }

    let config = web::Data::new(config);

    println!(
        "Exporter service is starting at http://{}:{}/metrics",
        args.address, args.port
    );

    actix_web::rt::System::new().block_on(async {
        if let Some(keep_alive_config) = keep_alive_config {
            actix_web::rt::spawn(async move {
                let mut interval =
                    actix_web::rt::time::interval(Duration::from_secs(keep_alive_config.interval));
                let client = Client::builder()
                    .build()
                    .expect("Failed to send http request for alive checker");
                let count = keep_alive_config.item.iter().count();

                loop {
                    for item in keep_alive_config.item.iter() {
                        let client = &client;
                        // FIXME: Find a method to send request concurrently
                        // It seems reqwest doesn't support concurrent call for Client. If any Future fails, all futures before synchronization point will fail.
                        let response = client
                            .get(Url::parse(&item.url).expect("Illegal url"))
                            .timeout(Duration::from_secs_f64(
                                keep_alive_config.interval as f64 / count as f64,
                            ))
                            .send()
                            .await;
                        let status = response
                            .and_then(|x| Ok(x.status().is_success()))
                            .unwrap_or(false);
                        alive_status.update(item, status)
                    }
                    interval.tick().await;
                }
            });
        }
        HttpServer::new(move || {
            App::new()
                .app_data(metrics.clone())
                .app_data(state.clone())
                .app_data(config.clone())
                .app_data(web::Data::new(Client::new()))
                .service(upstream_handler)
                .service(metrics_handler)
                .service(status_handler)
        })
        .workers(3)
        .bind(addr)?
        .run()
        .await
    })?;

    Ok(())
}

fn build_registry(
    metrics: &web::Data<metrics::Metrics>,
    alive_status: &web::Data<metrics::AliveStatus>,
) -> Registry {
    let mut registry = Registry::default();

    registry.register(
        "node_nvidia_driver_version",
        "Driver version of NVIDIA Driver",
        metrics.version.clone(),
    );
    registry.register(
        "node_nvidia_device_info",
        "Device information of NVIDIA GPU",
        metrics.device_info.clone(),
    );
    registry.register(
        "nvidia_fan_speed",
        "Fan speed of NVIDIA GPU",
        metrics.fan_speed.clone(),
    );
    registry.register(
        "node_nvidia_total_memory_bytes",
        "Total memory size of NVIDIA GPU",
        metrics.memory_total.clone(),
    );
    registry.register(
        "nvidia_used_memory_bytes",
        "Used memory size of NVIDIA GPU",
        metrics.memory_used.clone(),
    );
    registry.register(
        "node_nvidia_power_usage",
        "Power usage of NVIDIA GPU",
        metrics.power_usage.clone(),
    );
    registry.register(
        "node_nvidia_temperature_celsius",
        "Temperature of NVIDIA GPU",
        metrics.temperature.clone(),
    );
    registry.register(
        "node_nvidia_utilization_gpu_ratio",
        "GPU Utilization of NVIDIA GPU",
        metrics.utilization_gpu.clone(),
    );
    registry.register(
        "node_nvidia_utilization_memory_ratio",
        "Memory utilization of NVIDIA GPU",
        metrics.utilization_memory.clone(),
    );
    registry.register(
        "node_nvidia_user_used_memory_bytes",
        "User utilization of NVIDIA GPU",
        metrics.users_used_memory.clone(),
    );
    registry.register(
        "node_nvidia_user_cards",
        "Count of GPUs used by a user",
        metrics.users_used_cards.clone(),
    );
    registry.register(
        "node_home_folder_size_bytes",
        "Folder size in bytes of a user's home directory",
        metrics.users_used_disk.clone(),
    );
    registry.register(
        "node_alive_status",
        "Alive status of machine",
        alive_status.alive_status.clone(),
    );

    registry
}

#[get("/metrics")]
async fn metrics_handler(
    state: web::Data<Mutex<AppState>>,
    metrics: web::Data<metrics::Metrics>,
    http_client: web::Data<Client>,
    config: web::Data<AppReadOnlyConfig>,
) -> actix_web::Result<HttpResponse> {
    let mut body = {
        let mut state = state.lock().unwrap();
        metrics
            .update(&mut state.collector)
            .http_error("metric update failed", StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut body: String = String::new();
        encode(&mut body, &state.registry).unwrap();
        body
    };

    if config.upstream {
        let response: reqwest::Response = http_client
            .get(format!("http://127.0.0.1:{}/metrics", config.upstream_port))
            .send()
            .await
            .http_internal_error("Failed to get upstream data")?;
        if response.error_for_status_ref().is_err() {
            return Ok(HttpResponse::InternalServerError().body("Failed to fetch upstream data"));
        }
        body = response
            .text()
            .await
            .http_internal_error("Failed to parse upstream data")?
            + &body;
    }

    Ok(HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4; charset=utf-8")
        .body(body))
}

#[get("/")]
async fn upstream_handler(
    http_client: web::Data<Client>,
    config: web::Data<AppReadOnlyConfig>,
) -> actix_web::Result<HttpResponse> {
    if !config.upstream {
        return Ok(HttpResponse::NotFound().into());
    }

    let response: reqwest::Response = http_client
        .get(format!("http://127.0.0.1:{}", config.upstream_port))
        .send()
        .await
        .http_internal_error("Failed to get upstream data")?;
    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(
            response
                .text()
                .await
                .http_internal_error("Failed to get upstream data")?,
        ))
}

#[get("/status")]
async fn status_handler() -> actix_web::Result<HttpResponse> {
    Ok(HttpResponse::Ok().finish())
}
