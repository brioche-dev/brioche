use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::LogExporter;
use opentelemetry_otlp::SpanExporter;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::SdkTracer;
use opentelemetry_sdk::trace::SdkTracerProvider;

pub struct OtelProvider {
    logger_provider: SdkLoggerProvider,
    tracer_provider: SdkTracerProvider,
}

impl OtelProvider {
    pub fn new() -> anyhow::Result<Self> {
        let otel_enabled = matches!(
            std::env::var("BRIOCHE_ENABLE_OTEL").as_deref(),
            Ok("1" | "true")
        );

        let resource = Self::new_resource();

        let logger_provider = Self::new_logger_provider(otel_enabled, &resource)?;
        let tracer_provider = Self::new_tracer_provider(otel_enabled, &resource)?;

        Ok(Self {
            logger_provider,
            tracer_provider,
        })
    }

    #[must_use]
    pub const fn get_logger_provider(&self) -> &SdkLoggerProvider {
        &self.logger_provider
    }

    #[must_use]
    pub fn get_tracer(&self) -> SdkTracer {
        self.tracer_provider.tracer("brioche")
    }

    fn new_resource() -> opentelemetry_sdk::Resource {
        opentelemetry_sdk::Resource::builder()
            .with_service_name("brioche")
            .with_attribute(opentelemetry::KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .build()
    }

    pub fn new_logger_provider(
        otel_enabled: bool,
        resource: &opentelemetry_sdk::Resource,
    ) -> anyhow::Result<SdkLoggerProvider> {
        let logger_provider_builder = SdkLoggerProvider::builder();

        let logger_provider = if otel_enabled {
            let exporter = LogExporter::builder().with_http().build()?;

            logger_provider_builder
                .with_resource(resource.clone())
                .with_batch_exporter(exporter)
        } else {
            logger_provider_builder
        }
        .build();

        Ok(logger_provider)
    }

    pub fn new_tracer_provider(
        otel_enabled: bool,
        resource: &opentelemetry_sdk::Resource,
    ) -> anyhow::Result<SdkTracerProvider> {
        let tracer_provider_builder = SdkTracerProvider::builder();

        let tracer_provider = if otel_enabled {
            let exporter = SpanExporter::builder().with_http().build()?;

            tracer_provider_builder
                .with_resource(resource.clone())
                .with_batch_exporter(exporter)
        } else {
            tracer_provider_builder
        }
        .build();

        Ok(tracer_provider)
    }
}

impl Drop for OtelProvider {
    fn drop(&mut self) {
        let _ = self.logger_provider.shutdown();
        let _ = self.tracer_provider.shutdown();
    }
}
