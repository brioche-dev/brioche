#![expect(clippy::needless_pass_by_value)]

use std::{borrow::Cow, cell::RefCell, rc::Rc};

use deno_core::{OpState, v8};

use crate::script::runtime::{JsRuntimeError, StackFrame};

/// Cap on stack frames retained per recipe meta.
const MAX_STACK_FRAMES: usize = 6;

const OP_SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

deno_core::extension!(brioche_extension,
    ops = [
        op_brioche_version,
        op_brioche_console,
        op_brioche_utf8_encode,
        op_brioche_utf8_decode,
        op_brioche_tick_encode,
        op_brioche_tick_decode,
        op_brioche_stack_frames_from_exception,
    ],
    options = {
        worker_tx: super::JsRuntimeBridgeWorkerMessageSender,
    },
    state = |state, options| {
        state.put(options.worker_tx);
    },
);

#[deno_core::op2]
#[string(onebyte)]
const fn op_brioche_version() -> Cow<'static, [u8]> {
    Cow::Borrowed(crate::VERSION.as_bytes())
}

#[deno_core::op2(fast)]
fn op_brioche_console(
    #[string] level: &str,
    #[string] message: &str,
) -> Result<(), JsRuntimeError> {
    match level {
        "log" => tracing::info!("{message}"),
        "debug" => tracing::debug!("{message}"),
        "info" => tracing::info!("{message}"),
        "warn" => tracing::warn!("{message}"),
        "error" => tracing::error!("{message}"),
        _ => {
            return Err(JsRuntimeError::InvalidConsoleLogLevel {
                level: level.to_string(),
            });
        }
    }

    Ok(())
}

#[deno_core::op2]
fn op_brioche_utf8_encode<'a>(
    scope: &'a v8::PinScope,
    string: v8::Local<v8::String>,
) -> Result<v8::Local<'a, v8::Uint8Array>, JsRuntimeError> {
    let string = string.to_rust_string_lossy(scope);
    let backing_store = v8::ArrayBuffer::new_backing_store_from_vec(string.into_bytes());
    let buffer = v8::ArrayBuffer::with_backing_store(scope, &backing_store.make_shared());
    let array = v8::Uint8Array::new(scope, buffer, 0, buffer.byte_length())
        .ok_or(JsRuntimeError::FailedToCreate("Uint8Array".into()))?;
    Ok(array)
}

#[deno_core::op2]
#[string]
fn op_brioche_utf8_decode(bytes: v8::Local<v8::Uint8Array>) -> Result<String, JsRuntimeError> {
    let byte_length = bytes.byte_length();
    let mut buffer = vec![0; byte_length];
    let copied_length = bytes.copy_contents(&mut buffer);

    if copied_length != byte_length {
        return Err(JsRuntimeError::FailedToCopyBytes {
            expected: byte_length,
            actual: copied_length,
        });
    }

    let string = String::from_utf8(buffer)?;
    Ok(string)
}

#[deno_core::op2]
#[string]
fn op_brioche_tick_encode(bytes: v8::Local<v8::Uint8Array>) -> Result<String, JsRuntimeError> {
    let byte_length = bytes.byte_length();
    let mut buffer = vec![0; byte_length];
    let copied_length = bytes.copy_contents(&mut buffer);

    if copied_length != byte_length {
        return Err(JsRuntimeError::FailedToCopyBytes {
            expected: byte_length,
            actual: copied_length,
        });
    }

    let encoded = tick_encoding::encode(&buffer).into_owned();
    Ok(encoded)
}

#[deno_core::op2]
fn op_brioche_tick_decode<'a>(
    scope: &'a v8::PinScope,
    bytes: v8::Local<'a, v8::Uint8Array>,
) -> Result<v8::Local<'a, v8::Uint8Array>, JsRuntimeError> {
    let byte_length = bytes.byte_length();
    let mut buffer = vec![0; byte_length];
    let copied_length = bytes.copy_contents(&mut buffer);

    if copied_length != byte_length {
        return Err(JsRuntimeError::FailedToCopyBytes {
            expected: byte_length,
            actual: copied_length,
        });
    }

    let encoded = tick_encoding::decode(&buffer)?.into_owned();

    let backing_store = v8::ArrayBuffer::new_backing_store_from_vec(encoded);
    let encoded_buffer = v8::ArrayBuffer::with_backing_store(scope, &backing_store.make_shared());
    let encoded_array = v8::Uint8Array::new(scope, encoded_buffer, 0, encoded_buffer.byte_length())
        .ok_or(JsRuntimeError::FailedToCreate("Uint8Array".into()))?;
    Ok(encoded_array)
}

#[deno_core::op2(reentrant)]
#[serde]
fn op_brioche_stack_frames_from_exception<'a>(
    state: Rc<RefCell<OpState>>,
    scope: &mut v8::PinScope<'a, 'a>,
    exception: v8::Local<'a, v8::Value>,
) -> Result<Vec<StackFrame>, JsRuntimeError> {
    let worker_tx = state
        .borrow()
        .borrow::<super::JsRuntimeBridgeWorkerMessageSender>()
        .clone();

    let mut error = deno_core::error::JsError::from_v8_exception(scope, exception);
    error.frames.truncate(MAX_STACK_FRAMES);

    let (result_tx, result_rx) = std::sync::mpsc::channel();
    worker_tx
        .send(super::JsRuntimeBridgeWorkerMessage::EnrichStackFrames {
            frames: error.frames,
            result_tx,
        })
        .map_err(|_| JsRuntimeError::ChannelSendError {
            reason: "worker channel closed".into(),
        })?;

    let result = result_rx.recv_timeout(OP_SYNC_TIMEOUT).map_err(|error| {
        JsRuntimeError::ChannelRecvError {
            reason: error.to_string().into(),
        }
    })?;
    Ok(result)
}
