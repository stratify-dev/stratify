use crate::{AttrValue, MetricPoint, RunEvent};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::Resource;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// Resolved telemetry configuration. An empty `endpoint` means do not call this.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub service_name: String,
    pub namespace: Option<String>,
    pub version: String,
}

fn resource(config: &TelemetryConfig) -> Resource {
    let mut kvs = vec![
        KeyValue::new("service.name", config.service_name.clone()),
        KeyValue::new("service.version", config.version.clone()),
    ];
    if let Some(ns) = &config.namespace {
        kvs.push(KeyValue::new("service.namespace", ns.clone()));
    }
    Resource::new(kvs)
}

fn header_map(headers: &[(String, String)]) -> std::collections::HashMap<String, String> {
    headers.iter().cloned().collect()
}

/// Push metrics + the run event to the OTLP endpoint. Best-effort: returns Err
/// on any transport or build failure, never panics. Caller logs and continues.
pub fn emit(
    metrics: &[MetricPoint],
    event: &RunEvent,
    config: &TelemetryConfig,
) -> Result<(), Error> {
    use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};

    // --- Metrics ---
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(format!("{}/v1/metrics", config.endpoint.trim_end_matches('/')))
        .with_headers(header_map(&config.headers))
        .build()?;
    let reader = opentelemetry_sdk::metrics::PeriodicReaderWithOwnThread::builder(metric_exporter)
        .build();
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource(config))
        .build();
    let meter = meter_provider.meter("stratify");
    for point in metrics {
        let gauge = meter.f64_gauge(point.name.clone()).build();
        let attrs: Vec<KeyValue> = point
            .attributes
            .iter()
            .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
            .collect();
        gauge.record(point.value, &attrs);
    }
    meter_provider.force_flush()?;

    // --- Per-run event (log record) ---
    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(format!("{}/v1/logs", config.endpoint.trim_end_matches('/')))
        .with_headers(header_map(&config.headers))
        .build()?;
    let logger_provider = opentelemetry_sdk::logs::LoggerProvider::builder()
        .with_simple_exporter(log_exporter)
        .with_resource(resource(config))
        .build();
    {
        use opentelemetry::logs::{AnyValue as OtelAnyValue, LogRecord, Logger, LoggerProvider};
        let logger = logger_provider.logger("stratify");
        let mut record = logger.create_log_record();
        record.set_body(event.body.clone().into());
        for (k, v) in &event.attributes {
            let val: OtelAnyValue = match v {
                AttrValue::Str(s) => OtelAnyValue::from(s.clone()),
                AttrValue::Int(i) => OtelAnyValue::from(*i),
            };
            record.add_attribute(k.clone(), val);
        }
        logger.emit(record);
    }
    for result in logger_provider.force_flush() {
        result?;
    }

    let _ = meter_provider.shutdown();
    let _ = logger_provider.shutdown();
    Ok(())
}
