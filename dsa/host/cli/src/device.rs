//! `dsa settings device`, `dsa settings set device.<field>` and `dsa settings reset device`:
//! the device identity Android reports to its apps.

use dsa_host_core::device_identity::{DeviceIdentity, Field, IdentityReport, COMPUTER_KEYS};
use dsa_host_core::host_api::HostApiClient;
use serde_json::{json, Value};

use crate::Change;

/// `dsa settings set device.<field> <value>`.
pub fn parse_set(field: &str, value: &str) -> Result<Change, String> {
    let field = Field::parse(field)?;
    field.check(value)?;
    Ok(Change::Device(field, value.to_string()))
}

/// `dsa settings reset device` or `dsa settings reset device.<field>`.
pub fn parse_reset(name: &str) -> Result<Change, String> {
    if name == "device" {
        return Ok(Change::ResetDevice(None));
    }
    match name.strip_prefix("device.") {
        Some(field) => Ok(Change::ResetDevice(Some(Field::parse(field)?))),
        None => Err(format!(
            "dsa settings reset takes device or device.<field>, not {name:?}"
        )),
    }
}

/// An identity as one JSON object, with a key for every field.
pub fn identity_json(identity: &DeviceIdentity) -> Value {
    let mut object = serde_json::Map::new();
    for field in Field::ALL {
        object.insert(
            field.key().replace('.', "_"),
            Value::String(identity.get(field).to_string()),
        );
    }
    Value::Object(object)
}

/// An identity with the values that follow the computer, as one JSON object.
fn full_json(identity: &DeviceIdentity, report: &IdentityReport) -> Value {
    let mut object = identity_json(identity);
    if let Value::Object(map) = &mut object {
        for key in COMPUTER_KEYS {
            map.insert(
                key.to_string(),
                Value::String(report.computer_value(key).to_string()),
            );
        }
    }
    object
}

/// The report as `dsa settings device --json` prints it: what the user chose, what Android
/// reports, the defaults, and where each default came from (`computer` or `dsa`).
pub fn report_json(report: &IdentityReport) -> Value {
    let mut sources = serde_json::Map::new();
    for key in Field::ALL.into_iter().map(Field::key).chain(COMPUTER_KEYS) {
        sources.insert(
            key.replace('.', "_"),
            Value::String(report.origin(key).to_string()),
        );
    }
    json!({
        "chosen": identity_json(&report.chosen),
        "effective": full_json(&report.effective, report),
        "defaults": full_json(&report.defaults, report),
        "sources": Value::Object(sources),
    })
}

/// Where a default came from, for people.
fn origin_text(origin: &str) -> &'static str {
    if origin == "computer" {
        "from this computer"
    } else {
        "DSA's value"
    }
}

/// What the user chose and what Android reports, a line for each field.
pub fn describe(report: &IdentityReport) -> String {
    let mut lines: Vec<String> = Field::ALL
        .into_iter()
        .map(|field| {
            let value = report.effective.get(field);
            let value = if value.is_empty() {
                "(the image does not say)"
            } else {
                value
            };
            let source = if report.chosen.get(field).is_empty() {
                format!(" ({})", origin_text(report.origin(field.key())))
            } else {
                " (changed)".to_string()
            };
            format!("{}: {value}{source}", field.key())
        })
        .collect();
    for key in COMPUTER_KEYS {
        let value = report.computer_value(key);
        let value = if value.is_empty() {
            "(the computer does not say)"
        } else {
            value
        };
        lines.push(format!(
            "{key}: {value} ({})",
            origin_text(report.origin(key))
        ));
    }
    lines.push("Android reads these when it starts; a change applies from its next start.".into());
    lines.join("\n")
}

/// Shows the identity, after changing it as `change` says.
pub fn run(change: &Change, json: bool, client: &mut HostApiClient) -> Result<String, String> {
    let mut report = client.device_identity_report().map_err(|e| e.to_string())?;
    let chosen = report.chosen.clone();
    let wanted = match change {
        Change::Device(field, value) => {
            let mut wanted = chosen.clone();
            wanted.set(*field, value)?;
            Some(wanted)
        }
        Change::ResetDevice(Some(field)) => {
            let mut wanted = chosen.clone();
            wanted.reset(*field);
            Some(wanted)
        }
        Change::ResetDevice(None) => Some(DeviceIdentity::default()),
        _ => None,
    };
    if let Some(wanted) = wanted {
        report = client
            .set_device_identity_report(&wanted)
            .map_err(|e| e.to_string())?;
    }
    Ok(if json {
        report_json(&report).to_string()
    } else {
        describe(&report)
    })
}
