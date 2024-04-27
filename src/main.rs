mod metrics;
mod nvml_metrics;
mod utils;

use actix_web::http::header::ContentEncoding;
use actix_web::http::Uri;
use anyhow::{Context, Result};
use clap::Parser;
// use env_logger::Env;
use awc::Client;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use actix_web::{get, middleware, web, App, HttpResponse, HttpServer};
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

    let keep_alive_config = read_keep_alive_config(&args)?;

    let collector = NvmlMetricsCollector::new(args.show_all_users)?;
    let metrics = web::Data::new(metrics::Metrics::new());
    let alive_status = web::Data::new(metrics::AliveStatus::default());

    let registry = build_registry(&metrics, &alive_status);

    let state = web::Data::new(Mutex::new(AppState {
        registry,
        collector,
    }));

    let config = web::Data::new(AppReadOnlyConfig {
        upstream: args.combine_with_upstream,
        upstream_port: args.upstream_port,
    });

    if args.combine_with_upstream {
        println!("The upstream port is set as {}", args.upstream_port);
    }

    println!(
        "Exporter service is starting at http://{}:{}/metrics",
        args.address, args.port
    );

    actix_web::rt::System::new().block_on(async {
        if let Some(keep_alive_config) = keep_alive_config {
            actix_web::rt::spawn(async move {
                keep_alive_worker(keep_alive_config, alive_status).await
            });
        }

        HttpServer::new(move || {
            App::new()
                .wrap(middleware::Compress::default())
                .app_data(metrics.clone())
                .app_data(state.clone())
                .app_data(config.clone())
                .app_data(web::Data::new(Client::new()))
                .service(upstream_handler)
                .service(metrics_handler)
                .service(status_handler)
                .service(speedtest_handler)
        })
        .workers(2)
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
        "node_nvidia_driver_status", 
        "NVML is funcitonal",
        metrics.nvml_status.clone() 
    );
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
    let response: Option<_> = if config.upstream {
        let response = http_client
            .get(format!("http://127.0.0.1:{}/metrics", config.upstream_port))
            .send();

        Some(response)
    } else {
        None
    };

    let mut body: Vec<u8> = {
        let mut state = state.lock().unwrap();
        if let Err(e) = metrics.update(&mut state.collector) {
            eprintln!("Metric update failed: {}", e);
            metrics.clear();
        }
        // .http_error("metric update failed", StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut body: String = String::new();
        encode(&mut body, &state.registry).unwrap();
        body.into_bytes()
    };

    if let Some(response) = response {
        let mut response = response
            .await
            .http_internal_error("Failed to get upstream data")?;

        if response.status().is_server_error() {
            return Ok(HttpResponse::InternalServerError().body("Failed to fetch upstream data"));
        }
        body = [
            response
                .body()
                .await
                .http_internal_error("Failed to parse upstream data")?,
            body.into(),
        ]
        .concat();
    }
    Ok(HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4; charset=utf-8")
        .insert_header(("Access-Control-Allow-Origin", "*"))
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

    let mut response = http_client
        .get(format!("http://127.0.0.1:{}", config.upstream_port))
        .send()
        .await
        .http_internal_error("Failed to get upstream data")?;
    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(
            response
                .body()
                .await
                .http_internal_error("Failed to get upstream data")?,
        ))
}

#[get("/status")]
async fn status_handler() -> actix_web::Result<HttpResponse> {
    Ok(HttpResponse::Ok()
        .content_type("text/plain")
        .insert_header(("Access-Control-Allow-Origin", "*"))
        .body("ok"))
}

#[get("/speedtest")]
async fn speedtest_handler() -> actix_web::Result<HttpResponse> {
    let bytes = vec![0u8; 512 * 1024];
    Ok(HttpResponse::Ok()
        // .content_type("text/plain")
        .insert_header(ContentEncoding::Identity)
        .insert_header(("Access-Control-Allow-Origin", "*"))
        .body(bytes))
}

fn read_keep_alive_config(args: &Args) -> Result<Option<KeepAliveConfig>, anyhow::Error> {
    if args.alive_check {
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
        Ok(Some(keep_alive_config))
    } else {
        Ok(None)
    }
}

async fn keep_alive_worker(
    keep_alive_config: KeepAliveConfig,
    alive_status: web::Data<metrics::AliveStatus>,
) -> ! {
    let mut interval =
        actix_web::rt::time::interval(Duration::from_secs(keep_alive_config.interval));
    let client = Client::new();
    let count = keep_alive_config.item.len();

    loop {
        let mut responses = Vec::new();
        for item in keep_alive_config.item.iter() {
            // FIXME: Find a method to send request concurrently
            // It seems reqwest doesn't support concurrent call for Client. If any Future fails, all futures before synchronization point will fail.
            let response = client
                .get(&item.url)
                .timeout(Duration::from_secs_f64(
                    keep_alive_config.interval as f64 / count as f64,
                ))
                .send()
                .await;
            responses.push((item, response));
        }

        for (item, future) in responses {
            let response = future;
            let status = response.map(|x| x.status().is_success()).unwrap_or(false);
            alive_status.update(item, status)
        }

        interval.tick().await;
    }
}
