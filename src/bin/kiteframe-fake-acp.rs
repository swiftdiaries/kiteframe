use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

fn main() {
    if let Err(error) = run() {
        eprintln!("kiteframe-fake-acp: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let script_path = PathBuf::from(
        std::env::var_os("KITEFRAME_FAKE_ACP_SCRIPT")
            .ok_or("KITEFRAME_FAKE_ACP_SCRIPT is required")?,
    );
    let observation_path = std::env::var_os("KITEFRAME_FAKE_ACP_OBSERVATION").map(PathBuf::from);
    let allow_registration_eof =
        std::env::var_os("KITEFRAME_FAKE_ACP_ALLOW_REGISTRATION_EOF").is_some();
    let script: Vec<Value> = serde_json::from_slice(
        &std::fs::read(&script_path).map_err(|error| format!("could not read script: {error}"))?,
    )
    .map_err(|error| format!("invalid script: {error}"))?;
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let mut saved_ids = BTreeMap::<String, Value>::new();
    let mut observation = json!({
        "selectedPermissionOption": null,
        "permissionResponseCount": 0,
        "cancelCount": 0,
        "methods": [],
        "promptBlockCount": null,
        "promptText": null,
        "scriptComplete": false,
        "environmentKeys": std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .collect::<Vec<_>>(),
    });

    for (index, entry) in script.iter().enumerate() {
        if let Some(code) = entry.get("exitCode").and_then(Value::as_i64) {
            save_observation(observation_path.as_ref(), &observation)?;
            std::process::exit(code as i32);
        }
        if let Some(millis) = entry.get("sleepMillis").and_then(Value::as_u64) {
            std::thread::sleep(Duration::from_millis(millis));
            continue;
        }
        if let Some(raw) = entry.get("rawStdout").and_then(Value::as_str) {
            output
                .write_all(raw.as_bytes())
                .and_then(|()| output.write_all(b"\n"))
                .and_then(|()| output.flush())
                .map_err(|error| error.to_string())?;
            continue;
        }
        if let Some(notification) = entry.get("notify") {
            write_frame(
                &mut output,
                json!({
                    "jsonrpc": "2.0",
                    "method": required(notification, "method")?,
                    "params": notification.get("params").cloned().unwrap_or_else(|| json!({})),
                }),
            )?;
            continue;
        }
        if let Some(request) = entry.get("request") {
            let id = required(request, "id")?.clone();
            write_frame(
                &mut output,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": required(request, "method")?,
                    "params": request.get("params").cloned().unwrap_or_else(|| json!({})),
                }),
            )?;
            let Some(mut response) = read_frame(&mut input, index, false)? else {
                unreachable!("EOF is never allowed while awaiting a request response");
            };
            if entry
                .get("allowCancelBeforeResponse")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && response.get("method").and_then(Value::as_str) == Some("session/cancel")
            {
                observation["cancelCount"] =
                    json!(observation["cancelCount"].as_u64().unwrap_or(0) + 1);
                observation["methods"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("session/cancel"));
                save_observation(observation_path.as_ref(), &observation)?;
                let Some(cancel_response) = read_frame(&mut input, index, false)? else {
                    unreachable!("EOF is never allowed while awaiting a permission response");
                };
                response = cancel_response;
            }
            if let Some(expected) = entry.get("expectResponse") {
                assert_partial(expected, &response, "response")?;
            }
            observation["permissionResponseCount"] =
                json!(observation["permissionResponseCount"].as_u64().unwrap_or(0) + 1);
            observation["selectedPermissionOption"] = response
                .pointer("/result/outcome/optionId")
                .cloned()
                .unwrap_or(Value::Null);
            save_observation(observation_path.as_ref(), &observation)?;
            continue;
        }
        if let Some(name) = entry.get("respondTo").and_then(Value::as_str) {
            let id = saved_ids
                .get(name)
                .ok_or_else(|| format!("unknown saved id {name}"))?
                .clone();
            write_result(
                &mut output,
                id,
                entry.get("respond").cloned().unwrap_or(Value::Null),
            )?;
            continue;
        }
        let expected = entry
            .get("expect")
            .ok_or_else(|| format!("script entry {index} has no action"))?;
        let registration_eof = allow_registration_eof
            && expected.get("method").and_then(Value::as_str) == Some("session/new")
            && observation["methods"] == json!(["initialize"]);
        let Some(frame) = read_frame(&mut input, index, registration_eof)? else {
            return Ok(());
        };
        assert_partial(expected, &frame, "request")?;
        if let Some(method) = frame.get("method").and_then(Value::as_str) {
            observation["methods"]
                .as_array_mut()
                .unwrap()
                .push(json!(method));
            if method == "session/cancel" {
                observation["cancelCount"] =
                    json!(observation["cancelCount"].as_u64().unwrap_or(0) + 1);
            }
            if method == "session/prompt" {
                observation["promptBlockCount"] = frame
                    .pointer("/params/prompt")
                    .and_then(Value::as_array)
                    .map_or(Value::Null, |blocks| json!(blocks.len()));
                observation["promptText"] = frame
                    .pointer("/params/prompt/0/text")
                    .cloned()
                    .unwrap_or(Value::Null);
            }
        }
        save_observation(observation_path.as_ref(), &observation)?;
        if let Some(name) = entry.get("saveId").and_then(Value::as_str) {
            saved_ids.insert(
                name.to_owned(),
                frame.get("id").cloned().ok_or("saved request has no id")?,
            );
        }
        if let Some(code) = entry.get("exitAfterRead").and_then(Value::as_i64) {
            std::process::exit(code as i32);
        }
        if let Some(result) = entry.get("respond") {
            write_result(
                &mut output,
                frame
                    .get("id")
                    .cloned()
                    .ok_or("response target has no id")?,
                result.clone(),
            )?;
            if entry
                .get("duplicateResponse")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                write_result(
                    &mut output,
                    frame
                        .get("id")
                        .cloned()
                        .ok_or("duplicate response target has no id")?,
                    result.clone(),
                )?;
            }
        }
    }
    observation["scriptComplete"] = Value::Bool(true);
    save_observation(observation_path.as_ref(), &observation)?;
    let mut trailing = String::new();
    if input
        .read_line(&mut trailing)
        .map_err(|error| format!("final EOF read failed: {error}"))?
        != 0
    {
        return Err("received JSON-RPC input after script completion".to_owned());
    }
    Ok(())
}

fn read_frame(
    input: &mut impl BufRead,
    index: usize,
    allow_eof: bool,
) -> Result<Option<Value>, String> {
    let mut line = String::new();
    if input
        .read_line(&mut line)
        .map_err(|error| format!("read failed: {error}"))?
        == 0
    {
        return if allow_eof {
            Ok(None)
        } else {
            Err(format!("unexpected EOF at script entry {index}"))
        };
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(|error| format!("invalid JSON-RPC input: {error}"))
}

fn write_result(output: &mut impl Write, id: Value, result: Value) -> Result<(), String> {
    write_frame(output, json!({"jsonrpc":"2.0", "id":id, "result":result}))
}

fn write_frame(output: &mut impl Write, frame: Value) -> Result<(), String> {
    serde_json::to_writer(&mut *output, &frame).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}

fn required<'a>(value: &'a Value, field: &str) -> Result<&'a Value, String> {
    value
        .get(field)
        .ok_or_else(|| format!("script field {field} is required"))
}

fn assert_partial(expected: &Value, actual: &Value, path: &str) -> Result<(), String> {
    match expected {
        Value::Object(expected) => {
            let actual = actual
                .as_object()
                .ok_or_else(|| format!("{path} is not an object"))?;
            for (key, expected) in expected {
                let actual = actual
                    .get(key)
                    .ok_or_else(|| format!("{path}.{key} is absent"))?;
                assert_partial(expected, actual, &format!("{path}.{key}"))?;
            }
            Ok(())
        }
        _ if expected == actual => Ok(()),
        _ => Err(format!(
            "{path} drifted: expected {expected}, observed {actual}"
        )),
    }
}

fn save_observation(path: Option<&PathBuf>, observation: &Value) -> Result<(), String> {
    if let Some(path) = path {
        std::fs::write(path, serde_json::to_vec(observation).unwrap())
            .map_err(|error| format!("could not write observation: {error}"))?;
    }
    Ok(())
}
