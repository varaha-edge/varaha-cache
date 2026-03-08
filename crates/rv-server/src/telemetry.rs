use std::sync::Arc;
use std::sync::atomic::Ordering;

use opentelemetry::metrics::MeterProvider;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use rv_cache::CacheStats;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Holds all OTEL providers so they can be shut down cleanly on exit.
pub struct TelemetryProviders {
    pub tracer: SdkTracerProvider,
    pub meter: SdkMeterProvider,
    pub logger: SdkLoggerProvider,
}

impl TelemetryProviders {
    pub fn shutdown(self) {
        self.tracer.shutdown().ok();
        self.meter.shutdown().ok();
        self.logger.shutdown().ok();
    }
}

fn build_resource() -> Resource {
    Resource::builder()
        .with_service_name("varaha-cache")
        .build()
}

/// Initialize all three OTEL signals: traces, metrics, and logs.
///
/// Uses OTLP HTTP transport so it works via Envoy Gateway at
/// `https://otel.intra.varaha.io` -> Alloy -> Tempo/Mimir/Loki.
pub fn init(otel_endpoint: &str) -> TelemetryProviders {
    let resource = build_resource();

    // --- Traces ---
    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(otel_endpoint)
        .build()
        .expect("failed to create OTLP span exporter");

    let tracer_provider = SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(span_exporter)
        .build();

    let tracer = tracer_provider.tracer("varaha-cache");

    // --- Metrics ---
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(otel_endpoint)
        .build()
        .expect("failed to create OTLP metric exporter");

    let meter_provider = SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_periodic_exporter(metric_exporter)
        .build();

    // --- Logs ---
    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(otel_endpoint)
        .build()
        .expect("failed to create OTLP log exporter");

    let logger_provider = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(log_exporter)
        .build();

    let otel_log_layer =
        opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&logger_provider);

    // --- Compose tracing subscriber ---
    // Use per-layer filtering so OTEL layers receive all events independently
    // of the console filter which suppresses noisy SDK export logs.
    let base_filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let fmt_filter = if base_filter.contains("opentelemetry_sdk") {
        base_filter
    } else {
        format!("{base_filter},opentelemetry_sdk=off")
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(tracing_subscriber::EnvFilter::new(fmt_filter)),
        )
        .with(
            OpenTelemetryLayer::new(tracer).with_filter(tracing_subscriber::EnvFilter::new(
                "info,opentelemetry_sdk=off",
            )),
        )
        .with(
            otel_log_layer.with_filter(tracing_subscriber::EnvFilter::new(
                "info,opentelemetry_sdk=off",
            )),
        )
        .init();

    TelemetryProviders {
        tracer: tracer_provider,
        meter: meter_provider,
        logger: logger_provider,
    }
}

/// Register observable instruments that read from CacheStats atomics.
///
/// Call this after the CacheEngine is created so we have access to its stats.
/// The OTEL SDK calls the callbacks periodically (default 60s) to collect values.
pub fn register_cache_metrics(provider: &SdkMeterProvider, stats: Arc<CacheStats>) {
    let meter = provider.meter("varaha-cache");

    // Counters (monotonically increasing values)
    let s = Arc::clone(&stats);
    let _cache_hits = meter
        .u64_observable_counter("varaha_cache_hits_total")
        .with_description("Total cache hits")
        .with_callback(move |observer| {
            observer.observe(s.cache_hit.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _cache_misses = meter
        .u64_observable_counter("varaha_cache_misses_total")
        .with_description("Total cache misses")
        .with_callback(move |observer| {
            observer.observe(s.cache_miss.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _cache_pass = meter
        .u64_observable_counter("varaha_cache_pass_total")
        .with_description("Total pass-through requests (uncacheable)")
        .with_callback(move |observer| {
            observer.observe(s.cache_pass.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _cache_grace = meter
        .u64_observable_counter("varaha_cache_grace_hits_total")
        .with_description("Total grace hits (stale object served)")
        .with_callback(move |observer| {
            observer.observe(s.cache_hit_grace.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _cache_hfp = meter
        .u64_observable_counter("varaha_cache_hit_for_pass_total")
        .with_description("Total hit-for-pass (negative caching)")
        .with_callback(move |observer| {
            observer.observe(s.cache_hit_for_pass.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _backend_fetches = meter
        .u64_observable_counter("varaha_cache_backend_fetches_total")
        .with_description("Total backend fetch requests")
        .with_callback(move |observer| {
            observer.observe(s.backend_fetches.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _evictions = meter
        .u64_observable_counter("varaha_cache_evictions_total")
        .with_description("Total cache object evictions")
        .with_callback(move |observer| {
            observer.observe(s.evictions.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _expired = meter
        .u64_observable_counter("varaha_cache_expired_total")
        .with_description("Total expired objects")
        .with_callback(move |observer| {
            observer.observe(s.n_expired.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _purged = meter
        .u64_observable_counter("varaha_cache_purged_total")
        .with_description("Total purged objects")
        .with_callback(move |observer| {
            observer.observe(s.n_purged.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _bans_added = meter
        .u64_observable_counter("varaha_cache_bans_added_total")
        .with_description("Total ban expressions added")
        .with_callback(move |observer| {
            observer.observe(s.bans_added.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _bans_checked = meter
        .u64_observable_counter("varaha_cache_bans_checked_total")
        .with_description("Total ban checks performed")
        .with_callback(move |observer| {
            observer.observe(s.bans_checked.load(Ordering::Relaxed), &[]);
        })
        .build();

    // Gauges (current values that go up and down)
    let s = Arc::clone(&stats);
    let _objects = meter
        .u64_observable_gauge("varaha_cache_objects")
        .with_description("Current number of cached objects")
        .with_callback(move |observer| {
            observer.observe(s.n_objects.load(Ordering::Relaxed), &[]);
        })
        .build();

    let s = Arc::clone(&stats);
    let _bytes = meter
        .u64_observable_gauge("varaha_cache_bytes_stored")
        .with_description("Current bytes stored in cache")
        .with_unit("By")
        .with_callback(move |observer| {
            observer.observe(s.bytes_stored.load(Ordering::Relaxed), &[]);
        })
        .build();

    // Computed gauge: hit rate percentage
    let s = Arc::clone(&stats);
    let _hit_rate = meter
        .f64_observable_gauge("varaha_cache_hit_rate")
        .with_description("Cache hit rate percentage")
        .with_unit("%")
        .with_callback(move |observer| {
            let hit = s.cache_hit.load(Ordering::Relaxed);
            let miss = s.cache_miss.load(Ordering::Relaxed);
            let pass = s.cache_pass.load(Ordering::Relaxed);
            let total = hit + miss + pass;
            let rate = if total == 0 {
                0.0
            } else {
                (hit as f64 / total as f64) * 100.0
            };
            observer.observe(rate, &[]);
        })
        .build();
}
