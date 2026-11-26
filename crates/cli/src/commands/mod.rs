//! The S1 commands: one file per verb, each = envelope in, rendered text
//! out. Shared plumbing (dial, correlate, unwrap the success/failure union)
//! lives in [`ask`].

pub mod config;
pub mod daemon;
pub mod ledger;
pub mod logs;
pub mod permissions;
pub mod run;
pub mod sessions;
pub mod setup;
pub mod status;
pub mod usage;

use std::path::Path;

use openbutler_protocol::{DaemonRequest, DaemonResponse};
use serde_json::Value;

use crate::CliError;

/// Sends one stateless action and returns the daemon's `result` payload.
///
/// Connection, correlation and envelope validation happen in the client;
/// here the success/failure union splits into `Ok(result)` or the daemon's
/// own refusal sentence.
pub async fn ask(data_dir: &Path, action: &str) -> Result<Value, CliError> {
    let mut client = crate::client::Client::connect(data_dir).await?;
    let request = DaemonRequest::new(uuid::Uuid::new_v4().to_string(), action);
    match client.call(&request).await? {
        DaemonResponse::Success { result, .. } => Ok(result),
        DaemonResponse::Failure { error, .. } => Err(CliError::Answer(error)),
    }
}

/// One stateless action carrying a single payload field.
pub async fn ask_with(
    data_dir: &Path,
    action: &str,
    field: (&str, Value),
) -> Result<Value, CliError> {
    let mut client = crate::client::Client::connect(data_dir).await?;
    daemon::call_fields(&mut client, action, vec![field]).await
}
