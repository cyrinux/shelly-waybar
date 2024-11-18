use clap::{Parser, ValueEnum};
use futures_util::StreamExt;
use notify_rust::Notification;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, fs, io, thread, time::Duration};
use strum_macros::{Display, EnumString};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, required = true, env = "SHELLY_AUTH_KEY")]
    auth_key: String,
    #[arg(short, long, required = true, num_args(1..))]
    devices: Vec<String>,
    #[arg(
        short,
        long,
        default_value = "shelly-001-eu.shelly.cloud",
        env = "SHELLY_BASE_URL"
    )]
    server: String,
    #[arg(short, long, default_value = " | ")]
    waybar_separator: String,
    #[arg(short, long, default_value_t = 30)]
    interval: u64,
    #[arg(long, default_value = "long", value_enum)]
    format: OutputFormat,
    #[arg(short, long, default_value = "C", value_parser = ["C", "F"])]
    unit: String,
}

#[derive(Debug, Clone, ValueEnum, EnumString)]
#[strum(serialize_all = "lowercase")]
enum OutputFormat {
    Short,
    Long,
    Icons,
}

#[derive(Debug, EnumString, Display, PartialEq)]
#[strum(serialize_all = "lowercase")]
enum DeviceType {
    Temperature,
    Plug,
    Door,
    Window,
}

#[derive(Deserialize, Debug)]
struct ShellyResponse {
    isok: bool,
    errors: Option<Value>,
    data: Option<ShellyData>,
}

#[derive(Deserialize, Debug)]
struct ShellyData {
    device_status: Option<Value>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    let server = resolve_input(&args.server)?;
    let auth_key = resolve_input(&args.auth_key)?;

    let shared_statuses = Arc::new(Mutex::new(HashMap::new()));
    let client = Arc::new(Client::new());

    let websocket_listener = {
        let shared_statuses = Arc::clone(&shared_statuses);
        tokio::spawn(start_websocket_listener(
            &server,
            &auth_key,
            shared_statuses.clone(),
        ))
    };

    let polling_loop = {
        let shared_statuses = Arc::clone(&shared_statuses);
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            periodic_polling(args, shared_statuses, client, &server, &auth_key).await
        })
    };

    tokio::try_join!(websocket_listener, polling_loop)?;

    Ok(())
}

fn resolve_input(input: &str) -> Result<String, io::Error> {
    if Path::new(input).exists() {
        let value = fs::read_to_string(input)?.trim().to_string();
        Ok(value)
    } else {
        Ok(input.to_string())
    }
}

async fn periodic_polling(
    args: Args,
    shared_statuses: Arc<Mutex<HashMap<String, Value>>>,
    client: Arc<Client>,
    server: &str,
    auth_key: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        for device in &args.devices {
            if let Some(output) =
                fetch_device_status(client.clone(), server, device, auth_key).await
            {
                let mut statuses = shared_statuses.lock().unwrap();
                statuses.insert(device.to_string(), output);
            }
        }
        refresh_waybar_output(&shared_statuses, &args);
        thread::sleep(Duration::from_secs(args.interval));
    }
}

async fn fetch_device_status(
    client: Arc<Client>,
    server: &str,
    device: &str,
    auth_key: &str,
) -> Option<Value> {
    let full_url = format!("https://{}/device/status", server);
    let response = client
        .post(&full_url)
        .form(&[("id", device), ("auth_key", auth_key)])
        .send()
        .await
        .ok()?;

    let status: ShellyResponse = response.json().await.ok()?;

    if !status.isok {
        eprintln!(
            "Error fetching device status for {}: {:?}",
            device, status.errors
        );
        return None;
    }

    status.data?.device_status
}

async fn start_websocket_listener(
    server: &str,
    auth_key: &str,
    shared_statuses: Arc<Mutex<HashMap<String, Value>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws_url = format!("wss://{}:6113/shelly/wss/hk_sock?t={}", server, auth_key);

    eprintln!(
        "Connecting to WebSocket: {}",
        ws_url.replace(auth_key, "XXX")
    );

    let (ws_stream, _) = connect_async(&ws_url).await?;
    eprintln!("Connected to WebSocket.");

    let (_, mut read) = ws_stream.split();

    while let Some(message) = read.next().await {
        if let Ok(Message::Text(text)) = message {
            if let Ok(json) = serde_json::from_str::<Value>(&text) {
                if let Some(event) = json.get("event").and_then(|e| e.as_str()) {
                    match event {
                        "Shelly:StatusOnChange" => {
                            handle_status_on_change(&json, &shared_statuses);
                        }
                        "Shelly:Online" => {
                            handle_online_event(&json, &shared_statuses);
                        }
                        _ => {
                            eprintln!("Unknown event: {}", event);
                        }
                    }
                }
            }
        }
        refresh_waybar_output(&shared_statuses, &Args::parse());
    }

    Ok(())
}

fn handle_status_on_change(json: &Value, shared_statuses: &Arc<Mutex<HashMap<String, Value>>>) {
    if let Some(device) = json.get("device") {
        if let Some(device_id) = device.get("id").and_then(|id| id.as_str()) {
            if let Some(status) = json.get("status") {
                let mut statuses = shared_statuses.lock().unwrap();
                statuses.insert(device_id.to_string(), status.clone());
            }
        }
    }
}

fn handle_online_event(json: &Value, shared_statuses: &Arc<Mutex<HashMap<String, Value>>>) {
    if let Some(device) = json.get("device") {
        if let Some(device_id) = device.get("id").and_then(|id| id.as_str()) {
            if let Some(online) = json.get("online").and_then(|o| o.as_u64()) {
                let is_online = online == 1;

                let mut statuses = shared_statuses.lock().unwrap();
                statuses
                    .entry(device_id.to_string())
                    .or_insert_with(|| serde_json::json!({}))
                    .as_object_mut()
                    .unwrap()
                    .insert("online".to_string(), serde_json::Value::Bool(is_online));

                let state = if is_online { "Online" } else { "Offline" };
                Notification::new()
                    .summary(&format!("Device {} Status Changed", device_id))
                    .body(&format!("Device is now {}", state))
                    .show()
                    .unwrap();
            }
        }
    }
}

fn refresh_waybar_output(shared_statuses: &Arc<Mutex<HashMap<String, Value>>>, args: &Args) {
    let statuses = shared_statuses.lock().unwrap();
    let outputs: Vec<_> = statuses
        .values()
        .map(|status| generate_output(status.clone(), args))
        .collect();

    if outputs.is_empty() {
        eprintln!("Error: No valid device data found.");
    } else {
        let merged_text = outputs
            .iter()
            .map(|obj| obj["text"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(&args.waybar_separator);
        let merged_tooltip = outputs
            .iter()
            .map(|obj| obj["tooltip"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let merged_output = serde_json::json!({
            "text": merged_text,
            "tooltip": merged_tooltip
        });
        println!("{merged_output}");
    }
}

fn generate_output(status: Value, _args: &Args) -> Value {
    let is_online = status
        .get("online")
        .and_then(|o| o.as_bool())
        .unwrap_or(false);
    let online_text = if is_online { "🟢" } else { "🔴" };

    serde_json::json!({
        "text": format!("{} {}", online_text, "Device Status Placeholder"),
        "tooltip": format!("Device is currently {}", if is_online { "Online" } else { "Offline" })
    })
}

// Handle door status changes and notifications
fn handle_door_status(
    device_id: &str,
    device_name: Option<String>,
    device_status: &Value,
    door_status_map: &mut HashMap<String, bool>,
) -> Option<()> {
    let is_open = device_status["window:0"]["open"].as_bool().unwrap_or(false);
    let status_key = format!("{}:{}", device_id, device_name.clone().unwrap_or_default());

    if let Some(prev_status) = door_status_map.get(&status_key) {
        if *prev_status != is_open {
            let state = if is_open { "Open" } else { "Closed" };
            let name = device_name.unwrap_or_else(|| "Unnamed Door".to_string());
            Notification::new()
                .summary(&format!("Door Status Changed: {}", name))
                .body(&format!("The door is now {}", state))
                .show()
                .ok()?;
        }
    }

    door_status_map.insert(status_key, is_open);
    Some(())
}

// Parsing functions remain the same
fn parse_temperature_data(device_status: Value, format: OutputFormat, unit: &str) -> Value {
    let temp_c = device_status["temperature:0"]["tC"].as_f64().unwrap_or(0.0);
    let temp_f = device_status["temperature:0"]["tF"].as_f64().unwrap_or(0.0);
    let humidity = device_status["humidity:0"]["rh"].as_u64().unwrap_or(0);
    let battery = device_status["devicepower:0"]["battery"]["percent"]
        .as_u64()
        .unwrap_or(0);
    let rssi = device_status["reporter"]["rssi"].as_i64().unwrap_or(0);

    let (temp, unit_label) = if unit == "F" {
        (temp_f, "°F")
    } else {
        (temp_c, "°C")
    };

    match format {
        OutputFormat::Short => serde_json::json!({
            "text": format!("T: {:.1}{} H: {}%", temp, unit_label, humidity),
            "tooltip": format!("B: {}% RSSI: {}dBm", battery, rssi)
        }),
        OutputFormat::Long => serde_json::json!({
            "text": format!("Temp: {:.1}{} Humidity: {}%", temp, unit_label, humidity),
            "tooltip": format!("Battery: {}% RSSI: {}dBm", battery, rssi)
        }),
        OutputFormat::Icons => serde_json::json!({
            "text": format!("{:.1}{} 💧{}%", temp, unit_label, humidity),
            "tooltip": format!("🔋{}% 📶{}dBm", battery, rssi)
        }),
    }
}

fn parse_plug_data(device_status: Value, format: OutputFormat) -> Value {
    let power = device_status["switch:0"]["apower"].as_f64().unwrap_or(0.0);
    let voltage = device_status["switch:0"]["voltage"].as_f64().unwrap_or(0.0);
    let current = device_status["switch:0"]["current"].as_f64().unwrap_or(0.0);
    let output = device_status["switch:0"]["output"]
        .as_bool()
        .unwrap_or(false);
    let rssi = device_status["wifi"]["rssi"].as_i64().unwrap_or(0);

    let output_state = if output { "ON" } else { "OFF" };

    match format {
        OutputFormat::Short => serde_json::json!({
            "text": format!("P: {:.1}W V: {:.1}V", power, voltage),
            "tooltip": format!("I: {:.3}A RSSI: {}dBm O: {}", current, rssi, output_state)
        }),
        OutputFormat::Long => serde_json::json!({
            "text": format!("Power: {:.1}W Voltage: {:.1}V", power, voltage),
            "tooltip": format!("Current: {:.3}A WiFi RSSI: {}dBm Output: {}", current, rssi, output_state)
        }),
        OutputFormat::Icons => serde_json::json!({
            "text": format!("⚡{:.1}W 🔌{:.1}V", power, voltage),
            "tooltip": format!("🔋{:.3}A 📶{}dBm 🔆{}", current, rssi, output_state)
        }),
    }
}

fn parse_window_or_door_data(device_status: Value, is_window: bool, format: OutputFormat) -> Value {
    let is_open = device_status["window:0"]["open"].as_bool().unwrap_or(false);
    let lux = device_status["illuminance:0"]["lux"].as_u64().unwrap_or(0);
    let battery = device_status["devicepower:0"]["battery"]["percent"]
        .as_u64()
        .unwrap_or(0);
    let rssi = device_status["reporter"]["rssi"].as_i64().unwrap_or(0);

    let state = if is_open { "Open" } else { "Closed" };
    let tilt = if is_window {
        format!(
            ", Tilt: {}",
            device_status["tilt:0"]["angle"].as_u64().unwrap_or(0)
        )
    } else {
        "".to_string()
    };

    match format {
        OutputFormat::Short => serde_json::json!({
            "text": format!("{}: L: {}{}", state, lux, tilt),
            "tooltip": format!("B: {}% RSSI: {}dBm", battery, rssi)
        }),
        OutputFormat::Long => serde_json::json!({
            "text": format!("{}, Lux: {}{}", state, lux, tilt),
            "tooltip": format!("Battery: {}% RSSI: {}dBm", battery, rssi)
        }),
        OutputFormat::Icons => serde_json::json!({
            "text": format!("{} 🔆{}{}", if is_open { "🟢" } else { "🔴" }, lux, tilt),
            "tooltip": format!("🔋{}% 📶{}dBm", battery, rssi)
        }),
    }
}
