//! With no AWS credentials anywhere, S3 falls back to unsigned (public) access.

mod common;

use parquetry_engine::*;

#[test]
fn no_credentials_reads_public_data_only() {
    common::isolate_from_aws_credentials();
    let fx = common::Fixture::with_settings(EngineSettings::default());
    let error = fx.engine.s3_list("s3://".into()).wait().expect_err("listing buckets needs credentials");
    assert!(error.to_string().contains("only public buckets"), "{error}");
}
