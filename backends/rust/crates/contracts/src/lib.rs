//! Wire types shared by every Super Router component in Rust, strict parsing of untrusted JSON, and the
//! canonical JSON that feeds the idempotency digest.

mod datetime;
mod failure;
mod parse;
mod tools;
mod types;

pub use datetime::{parse_utc_millis, valid_datetime};
pub use failure::{classify as classify_failure, parse_retry_after, refusal, FailureClass, REFUSAL_BODY_LIMIT, RETRY_AFTER_MAX_MS};
pub use parse::{canonical, invalid_request, js_number, parse_invocation, parse_service};
pub use tools::{
    invocation_needs, is_js_space, is_json_object, js_stringify, js_trim, valid_image_url, valid_signature, valid_tool_call_id,
    valid_tool_name, Needs, ToolCalls,
};
pub use types::*;
