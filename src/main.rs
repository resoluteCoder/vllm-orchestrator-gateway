use std::{
    collections::HashMap,
    env,
    net::{IpAddr, SocketAddr},
};

use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use config::{validate_registered_detectors, DetectorConfig, GatewayConfig};
use serde::Serialize;
use serde_json::json;
use serde_json::{Map, Value};
use tower_http::trace::{self, TraceLayer};
use tracing::Level;

mod config;

#[derive(Debug, Serialize)]
struct OrchestratorDetector {
    input: HashMap<String, serde_json::Value>,
    // implement when output is completed, also need to see about splitting detectors in config to input/output
    // output: HashMap<String, serde_json::Value>,
}

fn get_orchestrator_detectors(
    detectors: Vec<String>,
    detector_config: Vec<DetectorConfig>,
) -> OrchestratorDetector {
    let mut input_detectors = HashMap::new();

    for detector in detector_config {
        if detectors.contains(&detector.name) && detector.detector_params.is_some() {
            input_detectors.insert(detector.name, detector.detector_params.unwrap());
        }
    }

    OrchestratorDetector {
        input: input_detectors,
    }
}

#[tokio::main]
async fn main() {
    let config_path = env::var("GATEWAY_CONFIG").unwrap_or("config/config.yaml".to_string());
    let gateway_config = config::read_config(&config_path);
    validate_registered_detectors(&gateway_config);

    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let mut app = Router::new().layer(
        TraceLayer::new_for_http()
            .make_span_with(trace::DefaultMakeSpan::new().level(Level::INFO))
            .on_response(trace::DefaultOnResponse::new().level(Level::INFO)),
    );

    for route in gateway_config.routes.iter() {
        let gateway_config = gateway_config.clone();
        let detectors = route.detectors.clone();
        let path = format!("/{}/v1/chat/completions", route.name);
        app = app.route(
            &path,
            post(|Json(payload): Json<serde_json::Value>| {
                handle_generation(Json(payload), detectors, gateway_config)
            }),
        );
        tracing::info!("exposed endpoints: {}", path);
    }

    let mut http_port = 8090;
    if let Ok(port) = env::var("HTTP_PORT") {
        match port.parse::<u16>() {
            Ok(port) => http_port = port,
            Err(err) => println!("{}", err),
        }
    }

    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

    let ip: IpAddr = host.parse().expect("Failed to parse host IP address");
    let addr = SocketAddr::from((ip, http_port));
    tracing::info!("listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn handle_generation(
    Json(mut payload): Json<serde_json::Value>,
    detectors: Vec<String>,
    gateway_config: GatewayConfig,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let orchestrator_detectors = get_orchestrator_detectors(detectors, gateway_config.detectors);

    let mut payload = payload.as_object_mut();

    let url = format!(
        "http://{}:{}/api/v2/chat/completions-detection",
        gateway_config.orchestrator.host, gateway_config.orchestrator.port
    );

    payload.as_mut().unwrap().insert(
        "detectors".to_string(),
        serde_json::to_value(&orchestrator_detectors).unwrap(),
    );
    let response_payload = orchestrator_post_request(payload, &url).await;

    Ok(Json(json!(response_payload.unwrap())).into_response())
}

async fn orchestrator_post_request(
    payload: Option<&mut Map<String, Value>>,
    url: &str,
) -> Result<serde_json::Value, anyhow::Error> {
    let client = reqwest::Client::new();
    let response = client.post(url).json(&payload).send();

    let json = response.await?.json().await?;
    println!("{:#?}", json);
    Ok(json)
}
