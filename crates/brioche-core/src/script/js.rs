use anyhow::Context as _;
use deno_core::v8;

use crate::recipe::StackFrame;

deno_core::extension!(
    brioche_js,
    ops = [
        op_brioche_version,
        op_brioche_console,
        op_brioche_stack_frames_from_exception,
        op_brioche_utf8_encode,
        op_brioche_utf8_decode,
        op_brioche_tick_encode,
        op_brioche_tick_decode,
    ],
);

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleLevel {
    Log,
    Debug,
    Info,
    Warn,
    Error,
}

#[deno_core::op]
fn op_brioche_version() -> &'static str {
    crate::VERSION
}

#[deno_core::op]
fn op_brioche_console(level: ConsoleLevel, message: String) {
    match level {
        ConsoleLevel::Log => tracing::info!("{}", message),
        ConsoleLevel::Debug => tracing::debug!("{}", message),
        ConsoleLevel::Info => tracing::info!("{}", message),
        ConsoleLevel::Warn => tracing::warn!("{}", message),
        ConsoleLevel::Error => tracing::error!("{}", message),
    }
}

#[deno_core::op2]
#[serde]
fn op_brioche_stack_frames_from_exception(
    scope: &mut v8::HandleScope,
    exception: v8::Local<v8::Value>,
) -> Vec<StackFrame> {
    let error = deno_core::error::JsError::from_v8_exception(scope, exception);
    error
        .frames
        .into_iter()
        .map(|frame| StackFrame {
            file_name: frame.file_name,
            line_number: frame.line_number,
            column_number: frame.column_number,
        })
        .collect()
}

#[deno_core::op2]
fn op_brioche_utf8_encode<'a>(
    scope: &'a mut v8::HandleScope,
    string: v8::Local<v8::String>,
) -> anyhow::Result<v8::Local<'a, v8::Uint8Array>> {
    let string = string.to_rust_string_lossy(scope);
    let backing_store = v8::ArrayBuffer::new_backing_store_from_vec(string.into_bytes());
    let buffer = v8::ArrayBuffer::with_backing_store(scope, &backing_store.make_shared());
    let array = v8::Uint8Array::new(scope, buffer, 0, buffer.byte_length())
        .context("failed to create Uint8Array")?;
    Ok(array)
}

#[deno_core::op2]
#[string]
fn op_brioche_utf8_decode(bytes: v8::Local<v8::Uint8Array>) -> anyhow::Result<String> {
    let byte_length = bytes.byte_length();
    let mut buffer = vec![0; byte_length];
    let copied_length = bytes.copy_contents(&mut buffer);
    anyhow::ensure!(copied_length == byte_length, "mismatch in copied bytes");
    let string = String::from_utf8(buffer).context("invalid UTF-8")?;
    Ok(string)
}

#[deno_core::op2]
#[string]
fn op_brioche_tick_encode(bytes: v8::Local<v8::Uint8Array>) -> anyhow::Result<String> {
    let byte_length = bytes.byte_length();
    let mut buffer = vec![0; byte_length];
    let copied_length = bytes.copy_contents(&mut buffer);
    anyhow::ensure!(copied_length == byte_length, "mismatch in copied bytes");
    let encoded = tick_encoding::encode(&buffer).into_owned();
    Ok(encoded)
}

#[deno_core::op2]
fn op_brioche_tick_decode<'a>(
    scope: &'a mut v8::HandleScope,
    bytes: v8::Local<'a, v8::Uint8Array>,
) -> anyhow::Result<v8::Local<'a, v8::Uint8Array>> {
    let byte_length = bytes.byte_length();
    let mut buffer = vec![0; byte_length];
    let copied_length = bytes.copy_contents(&mut buffer);
    anyhow::ensure!(copied_length == byte_length, "mismatch in copied bytes");

    let encoded = tick_encoding::decode(&buffer)?.into_owned();

    let backing_store = v8::ArrayBuffer::new_backing_store_from_vec(encoded);
    let encoded_buffer = v8::ArrayBuffer::with_backing_store(scope, &backing_store.make_shared());
    let encoded_array = v8::Uint8Array::new(scope, encoded_buffer, 0, encoded_buffer.byte_length())
        .context("failed to create Uint8Array")?;
    Ok(encoded_array)
}
