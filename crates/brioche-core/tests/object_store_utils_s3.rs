use assert_matches::assert_matches;
use aws_runtime::env_config::file::{EnvConfigFileKind, EnvConfigFiles};
use brioche_core::object_store_utils::AwsS3Config;
use object_store::CredentialProvider as _;

#[tokio::test]
async fn test_object_store_utils_s3_credentials_empty_fail() {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .load()
        .await;
    let credential_provider =
        brioche_core::object_store_utils::AwsS3CredentialProvider::new(aws_config);
    let result = credential_provider.get_credential().await;

    assert_matches!(result, Err(_));
}

#[tokio::test]
async fn test_object_store_utils_s3_credentials_explicit_credentials() {
    // Equivalent to setting credentials through env vars
    let aws_credentials = aws_credential_types::Credentials::for_tests();
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .credentials_provider(aws_credentials.clone())
        .load()
        .await;
    let credential_provider =
        brioche_core::object_store_utils::AwsS3CredentialProvider::new(aws_config);
    let credential = credential_provider.get_credential().await.unwrap();

    assert_eq!(credential.key_id, aws_credentials.access_key_id());
    assert_eq!(credential.secret_key, aws_credentials.secret_access_key());
    assert_matches!(credential.token, None);
}

#[tokio::test]
async fn test_object_store_utils_s3_credentials_explicit_credentials_with_session_token() {
    // Equivalent to setting credentials through env vars
    let aws_credentials = aws_credential_types::Credentials::for_tests_with_session_token();
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .credentials_provider(aws_credentials.clone())
        .load()
        .await;
    let credential_provider =
        brioche_core::object_store_utils::AwsS3CredentialProvider::new(aws_config);
    let credential = credential_provider.get_credential().await.unwrap();

    assert_eq!(credential.key_id, aws_credentials.access_key_id());
    assert_eq!(credential.secret_key, aws_credentials.secret_access_key());
    assert_eq!(credential.token.as_deref(), aws_credentials.session_token());
}

#[tokio::test]
async fn test_object_store_utils_s3_credentials_from_env_files() {
    let tempdir = tempdir::TempDir::new("brioche-test").unwrap();
    let credentials_path = tempdir.path().join("credentials");
    tokio::fs::write(
        &credentials_path,
        indoc::indoc! {"
            [default]
            aws_access_key_id = test_access_key_id
            aws_secret_access_key = test_secret_access_key
        "},
    )
    .await
    .unwrap();

    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .profile_files(
            EnvConfigFiles::builder()
                .with_file(EnvConfigFileKind::Credentials, &credentials_path)
                .build(),
        )
        .load()
        .await;
    let credential_provider =
        brioche_core::object_store_utils::AwsS3CredentialProvider::new(aws_config);
    let credential = credential_provider.get_credential().await.unwrap();

    assert_eq!(credential.key_id, "test_access_key_id");
    assert_eq!(credential.secret_key, "test_secret_access_key");
    assert_matches!(credential.token, None);
}

#[tokio::test]
async fn test_object_store_utils_s3_credentials_from_env_files_with_session_token() {
    let tempdir = tempdir::TempDir::new("brioche-test").unwrap();
    let credentials_path = tempdir.path().join("credentials");
    tokio::fs::write(
        &credentials_path,
        indoc::indoc! {"
            [default]
            aws_access_key_id = test_access_key_id
            aws_secret_access_key = test_secret_access_key
            aws_session_token = test_session_token
        "},
    )
    .await
    .unwrap();

    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .profile_files(
            EnvConfigFiles::builder()
                .with_file(EnvConfigFileKind::Credentials, &credentials_path)
                .build(),
        )
        .load()
        .await;
    let credential_provider =
        brioche_core::object_store_utils::AwsS3CredentialProvider::new(aws_config);
    let credential = credential_provider.get_credential().await.unwrap();

    assert_eq!(credential.key_id, "test_access_key_id");
    assert_eq!(credential.secret_key, "test_secret_access_key");
    assert_eq!(credential.token.as_deref(), Some("test_session_token"));
}

#[tokio::test]
async fn test_object_store_utils_s3_credentials_from_env_files_with_profile() {
    let tempdir = tempdir::TempDir::new("brioche-test").unwrap();
    let credentials_path = tempdir.path().join("credentials");
    tokio::fs::write(
        &credentials_path,
        indoc::indoc! {"
            [default]
            aws_access_key_id = test_access_key_id
            aws_secret_access_key = test_secret_access_key
            [custom_profile]
            aws_access_key_id = another_access_key_id
            aws_secret_access_key = another_secret_access_key
        "},
    )
    .await
    .unwrap();

    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .profile_files(
            EnvConfigFiles::builder()
                .with_file(EnvConfigFileKind::Credentials, &credentials_path)
                .build(),
        )
        .profile_name("custom_profile")
        .load()
        .await;
    let credential_provider =
        brioche_core::object_store_utils::AwsS3CredentialProvider::new(aws_config);
    let credential = credential_provider.get_credential().await.unwrap();

    assert_eq!(credential.key_id, "another_access_key_id");
    assert_eq!(credential.secret_key, "another_secret_access_key");
    assert_matches!(credential.token, None);
}

#[tokio::test]
async fn test_object_store_utils_s3_config_empty_default() {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .load()
        .await;
    let s3_config = brioche_core::object_store_utils::load_s3_config(&aws_config);

    assert_eq!(s3_config, AwsS3Config::default());
}

#[tokio::test]
async fn test_object_store_utils_s3_config_explicit_region() {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .region(aws_config::Region::from_static("us-east-2"))
        .load()
        .await;
    let s3_config = brioche_core::object_store_utils::load_s3_config(&aws_config);

    assert_eq!(
        s3_config,
        AwsS3Config {
            region: Some("us-east-2".to_string()),
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn test_object_store_utils_s3_config_explicit_endpoint() {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .endpoint_url("https://example.com")
        .load()
        .await;
    let s3_config = brioche_core::object_store_utils::load_s3_config(&aws_config);

    assert_eq!(
        s3_config,
        AwsS3Config {
            endpoint: Some("https://example.com".to_string()),
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn test_object_store_utils_s3_config_values_from_env_files() {
    let tempdir = tempdir::TempDir::new("brioche-test").unwrap();
    let config_path = tempdir.path().join("credentials");
    tokio::fs::write(
        &config_path,
        indoc::indoc! {"
            [default]
            region = us-west-2
            endpoint_url = https://example.com
        "},
    )
    .await
    .unwrap();

    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .profile_files(
            EnvConfigFiles::builder()
                .with_file(EnvConfigFileKind::Config, &config_path)
                .build(),
        )
        .load()
        .await;
    let s3_config = brioche_core::object_store_utils::load_s3_config(&aws_config);

    assert_eq!(
        s3_config,
        AwsS3Config {
            region: Some("us-west-2".to_string()),
            endpoint: Some("https://example.com".to_string()),
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn test_object_store_utils_s3_config_values_from_env_files_with_profile() {
    let tempdir = tempdir::TempDir::new("brioche-test").unwrap();
    let config_path = tempdir.path().join("credentials");
    tokio::fs::write(
        &config_path,
        indoc::indoc! {"
            [default]
            region = us-west-2
            endpoint_url = https://example.com
            [profile custom-profile]
            region = us-west-1
            endpoint_url = https://example.org
        "},
    )
    .await
    .unwrap();

    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .profile_files(
            EnvConfigFiles::builder()
                .with_file(EnvConfigFileKind::Config, &config_path)
                .build(),
        )
        .profile_name("custom-profile")
        .load()
        .await;
    let s3_config = brioche_core::object_store_utils::load_s3_config(&aws_config);

    assert_eq!(
        s3_config,
        AwsS3Config {
            region: Some("us-west-1".to_string()),
            endpoint: Some("https://example.org".to_string()),
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn test_object_store_utils_s3_config_values_from_env_files_with_service_config() {
    let tempdir = tempdir::TempDir::new("brioche-test").unwrap();
    let config_path = tempdir.path().join("credentials");
    tokio::fs::write(
        &config_path,
        indoc::indoc! {"
            [default]
            region = us-west-2
            endpoint_url = https://example.com
            [profile custom-profile]
            region = us-west-1
            endpoint_url = https://example.org
            services = custom-services
            [services custom-services]
            dynamodb =
                endpoint_url = https://dynamodb.example.com
            s3 =
                endpoint_url = https://s3.example.com
        "},
    )
    .await
    .unwrap();

    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .empty_test_environment()
        .profile_files(
            EnvConfigFiles::builder()
                .with_file(EnvConfigFileKind::Config, &config_path)
                .build(),
        )
        .profile_name("custom-profile")
        .load()
        .await;
    let s3_config = brioche_core::object_store_utils::load_s3_config(&aws_config);

    assert_eq!(
        s3_config,
        AwsS3Config {
            region: Some("us-west-1".to_string()),
            endpoint: Some("https://s3.example.com".to_string()),
            ..Default::default()
        }
    );
}
