use deno_core::v8;

use crate::brioche::value::StackFrame;

deno_core::extension!(
    brioche_js,
    ops = [op_brioche_console, op_brioche_stack_frames_from_exception]
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
