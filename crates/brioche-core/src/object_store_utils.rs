use std::sync::Arc;

use anyhow::Context as _;
use aws_credential_types::provider::ProvideCredentials as _;
use futures::StreamExt as _;

/// An implementation of [`object_store::ObjectStore`] that supports reading
/// from multiple stores and writing to a single store. Only `get` and `put`
/// operations are supported.
///
/// `get` operations will try each store from `read_layers` in order. A
/// result of `Err(NotFound)` will try the next layer, and `Ok(_)` or
/// any other error will return.
///
/// `put` operations will write to `write_layer` if set, otherwise the
/// operation will return an error.
#[derive(Debug, Clone)]
pub struct LayeredObjectStore {
    pub read_layers: Vec<Arc<dyn object_store::ObjectStore>>,
    pub write_layer: Option<Arc<dyn object_store::ObjectStore>>,
}

impl std::fmt::Display for LayeredObjectStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[async_trait::async_trait]
impl object_store::ObjectStore for LayeredObjectStore {
    async fn get_opts(
        &self,
        location: &object_store::path::Path,
        options: object_store::GetOptions,
    ) -> object_store::Result<object_store::GetResult> {
        for layer in &self.read_layers {
            let result = layer.get_opts(location, options.clone()).await;

            match result {
                Ok(result) => {
                    return Ok(result);
                }
                Err(object_store::Error::NotFound { .. }) => {
                    // Continue to next layer
                }
                Err(error) => {
                    return Err(error);
                }
            }
        }

        Err(object_store::Error::NotFound {
            path: location.to_string(),
            source: Box::new(NoLayerContainedObjectError),
        })
    }

    async fn put_opts(
        &self,
        location: &object_store::path::Path,
        payload: object_store::PutPayload,
        opts: object_store::PutOptions,
    ) -> object_store::Result<object_store::PutResult> {
        let Some(layer) = &self.write_layer else {
            return Err(object_store::Error::NotSupported {
                source: Box::new(NoWritableLayerError),
            });
        };

        layer.put_opts(location, payload, opts).await
    }

    async fn put_multipart_opts(
        &self,
        location: &object_store::path::Path,
        opts: object_store::PutMultipartOptions,
    ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
        let Some(layer) = &self.write_layer else {
            return Err(object_store::Error::NotSupported {
                source: Box::new(NoWritableLayerError),
            });
        };

        layer.put_multipart_opts(location, opts).await
    }

    fn delete_stream(
        &self,
        _locations: futures::stream::BoxStream<
            'static,
            object_store::Result<object_store::path::Path>,
        >,
    ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::path::Path>> {
        let implementer = self.to_string();
        futures::stream::once(async {
            Err(object_store::Error::NotImplemented {
                implementer,
                operation: "delete_stream".to_string(),
            })
        })
        .boxed()
    }

    fn list(
        &self,
        _prefix: Option<&object_store::path::Path>,
    ) -> futures::stream::BoxStream<'static, object_store::Result<object_store::ObjectMeta>> {
        let implementer = self.to_string();
        futures::stream::once(async {
            Err(object_store::Error::NotImplemented {
                implementer,
                operation: "list".to_string(),
            })
        })
        .boxed()
    }

    async fn list_with_delimiter(
        &self,
        _prefix: Option<&object_store::path::Path>,
    ) -> object_store::Result<object_store::ListResult> {
        Err(object_store::Error::NotImplemented {
            implementer: self.to_string(),
            operation: "list_with_delimiter".to_string(),
        })
    }

    async fn copy_opts(
        &self,
        _from: &object_store::path::Path,
        _to: &object_store::path::Path,
        _options: object_store::CopyOptions,
    ) -> object_store::Result<()> {
        Err(object_store::Error::NotImplemented {
            implementer: self.to_string(),
            operation: "copy_opts".to_string(),
        })
    }
}

pub const PUT_NUM_RETRIES: usize = 5;
pub const PUT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

pub async fn put_opts_with_retry(
    object_store: &impl object_store::ObjectStore,
    location: &object_store::path::Path,
    payload: object_store::PutPayload,
    opts: object_store::PutOptions,
    mut num_retries: usize,
    retry_delay: std::time::Duration,
) -> object_store::Result<object_store::PutResult> {
    loop {
        let result = object_store
            .put_opts(location, payload.clone(), opts.clone())
            .await;
        let error = match result {
            Ok(result) => {
                return Ok(result);
            }
            Err(
                error @ (object_store::Error::NotFound { .. }
                | object_store::Error::InvalidPath { .. }
                | object_store::Error::JoinError { .. }
                | object_store::Error::NotSupported { .. }
                | object_store::Error::AlreadyExists { .. }
                | object_store::Error::Precondition { .. }
                | object_store::Error::NotModified { .. }
                | object_store::Error::NotImplemented { .. }
                | object_store::Error::PermissionDenied { .. }
                | object_store::Error::Unauthenticated { .. }
                | object_store::Error::UnknownConfigurationKey { .. }),
            ) => {
                // These errors are all fatal and shouldn't be retried,
                // as we'd probably get the same error
                return Err(error);
            }
            Err(error @ (object_store::Error::Generic { .. } | _)) => {
                // Other error: default to assuming that it's retryable
                error
            }
        };

        let Some(retries_remaining) = num_retries.checked_sub(1) else {
            // No retries left, so return the last error
            return Err(error);
        };
        num_retries = retries_remaining;

        tracing::warn!(
            %location,
            retries_remaining,
            ?error,
            "object store PUT request failed, retrying"
        );
        tokio::time::sleep(retry_delay).await;
    }
}

#[derive(Debug)]
pub struct AwsS3CredentialProvider {
    config: aws_config::SdkConfig,
}

impl AwsS3CredentialProvider {
    #[must_use]
    pub const fn new(config: aws_config::SdkConfig) -> Self {
        Self { config }
    }

    async fn get_aws_sdk_credentials(&self) -> anyhow::Result<aws_credential_types::Credentials> {
        let credentials_provider = self
            .config
            .credentials_provider()
            .context("failed to get credentials provider from AWS SDK config")?;
        let credentials = credentials_provider
            .provide_credentials()
            .await
            .context("failed to load AWS credentials")?;
        Ok(credentials)
    }
}

#[async_trait::async_trait]
impl object_store::CredentialProvider for AwsS3CredentialProvider {
    type Credential = object_store::aws::AwsCredential;

    async fn get_credential(&self) -> object_store::Result<Arc<Self::Credential>> {
        let credentials = self.get_aws_sdk_credentials().await.map_err(|source| {
            object_store::Error::Generic {
                store: "AwsS3CredentialProvider",
                source: source.into(),
            }
        })?;

        Ok(Arc::new(object_store::aws::AwsCredential {
            key_id: credentials.access_key_id().to_string(),
            secret_key: credentials.secret_access_key().to_string(),
            token: credentials
                .session_token()
                .map(std::string::ToString::to_string),
        }))
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AwsS3Config {
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub allow_http: Option<bool>,
    pub virtual_hosted_style_request: Option<bool>,
    pub s3_express: Option<bool>,
    pub imdsv1_fallback: bool,
    pub unsigned_payload: Option<bool>,
    pub skip_signature: Option<bool>,
    pub checksum_algorithm: Option<object_store::aws::Checksum>,
    pub metadata_endpoint: Option<String>,
    pub proxy_url: Option<String>,
    pub proxy_ca_certificate: Option<String>,
    pub proxy_excludes: Option<String>,
    pub copy_if_not_exists: Option<object_store::aws::S3CopyIfNotExists>,
    pub conditional_put: Option<object_store::aws::S3ConditionalPut>,
    pub disable_tagging: Option<bool>,
    pub sse_kms_encryption: Option<String>,
    pub dsse_kms_encryption: Option<String>,
    pub ssec_encryption: Option<String>,
    pub bucket_key: Option<bool>,
    pub request_payer: Option<bool>,
}

#[must_use]
pub fn load_s3_config(config: &aws_config::SdkConfig) -> AwsS3Config {
    let region = config.region().map(std::string::ToString::to_string);

    let endpoint = if config.get_origin("endpoint_url").is_client_config() {
        config.endpoint_url().map(std::string::ToString::to_string)
    } else {
        config
            .service_config()
            .and_then(|service_config| {
                service_config.load_config(
                    aws_types::service_config::ServiceConfigKey::builder()
                        .service_id("s3")
                        .env("AWS_ENDPOINT_URL")
                        .profile("endpoint_url")
                        .build()
                        .unwrap(),
                )
            })
            .or_else(|| config.endpoint_url().map(std::string::ToString::to_string))
    };

    AwsS3Config {
        region,
        endpoint,
        ..Default::default()
    }
}

pub fn apply_s3_config(
    config: AwsS3Config,
    mut builder: object_store::aws::AmazonS3Builder,
) -> object_store::aws::AmazonS3Builder {
    let AwsS3Config {
        region,
        endpoint,
        allow_http,
        virtual_hosted_style_request,
        s3_express,
        imdsv1_fallback,
        unsigned_payload,
        skip_signature,
        checksum_algorithm,
        metadata_endpoint,
        proxy_url,
        proxy_ca_certificate,
        proxy_excludes,
        copy_if_not_exists,
        conditional_put,
        disable_tagging,
        sse_kms_encryption,
        dsse_kms_encryption,
        ssec_encryption,
        bucket_key,
        request_payer,
    } = config;

    if let Some(region) = region {
        builder = builder.with_region(region);
    }

    if let Some(endpoint) = endpoint {
        builder = builder.with_endpoint(endpoint);
    }

    if let Some(allow_http) = allow_http {
        builder = builder.with_allow_http(allow_http);
    }

    if let Some(virtual_hosted_style_request) = virtual_hosted_style_request {
        builder = builder.with_virtual_hosted_style_request(virtual_hosted_style_request);
    }

    if let Some(s3_express) = s3_express {
        builder = builder.with_s3_express(s3_express);
    }

    if imdsv1_fallback {
        builder = builder.with_imdsv1_fallback();
    }

    if let Some(unsigned_payload) = unsigned_payload {
        builder = builder.with_unsigned_payload(unsigned_payload);
    }

    if let Some(skip_signature) = skip_signature {
        builder = builder.with_skip_signature(skip_signature);
    }

    if let Some(checksum_algorithm) = checksum_algorithm {
        builder = builder.with_checksum_algorithm(checksum_algorithm);
    }

    if let Some(metadata_endpoint) = metadata_endpoint {
        builder = builder.with_metadata_endpoint(metadata_endpoint);
    }

    if let Some(proxy_url) = proxy_url {
        builder = builder.with_proxy_url(proxy_url);
    }

    if let Some(proxy_ca_certificate) = proxy_ca_certificate {
        builder = builder.with_proxy_ca_certificate(proxy_ca_certificate);
    }

    if let Some(proxy_excludes) = proxy_excludes {
        builder = builder.with_proxy_excludes(proxy_excludes);
    }

    if let Some(copy_if_not_exists) = copy_if_not_exists {
        builder = builder.with_copy_if_not_exists(copy_if_not_exists);
    }

    if let Some(conditional_put) = conditional_put {
        builder = builder.with_conditional_put(conditional_put);
    }

    if let Some(disable_tagging) = disable_tagging {
        builder = builder.with_disable_tagging(disable_tagging);
    }

    if let Some(sse_kms_encryption) = sse_kms_encryption {
        builder = builder.with_sse_kms_encryption(sse_kms_encryption);
    }

    if let Some(dsse_kms_encryption) = dsse_kms_encryption {
        builder = builder.with_dsse_kms_encryption(dsse_kms_encryption);
    }

    if let Some(ssec_encryption) = ssec_encryption {
        builder = builder.with_ssec_encryption(ssec_encryption);
    }

    if let Some(bucket_key) = bucket_key {
        builder = builder.with_bucket_key(bucket_key);
    }

    if let Some(request_payer) = request_payer {
        builder = builder.with_request_payer(request_payer);
    }

    builder
}

#[derive(Debug, thiserror::Error)]
#[error("object not found in LayeredObjectStore")]
struct NoLayerContainedObjectError;

#[derive(Debug, thiserror::Error)]
#[error("no writable layer configured for LayeredObjectStore")]
struct NoWritableLayerError;
