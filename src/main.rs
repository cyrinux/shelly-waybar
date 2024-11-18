use clap::{Parser, ValueEnum};
use notify_rust::Notification;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::{collections::HashMap, thread, time::Duration};
use strum_macros::{Display, EnumString};

#[derive(Parser, Debug)]
struct Args {
    /// List of devices in the format <device_type>:<device_id>:<device_name>
    #[arg(short, long, required = true, num_args(1..))]
    devices: Vec<String>,

    /// Auth key for the Shelly API (can also be set via the SHELLY_AUTH_KEY environment variable)
    #[arg(short, long, env = "SHELLY_AUTH_KEY")]
    auth_key: String,

    /// Base URL of the Shelly server
    #[arg(short, long, default_value = "https://shelly-001-eu.shelly.cloud")]
    base_url: String,

    /// Separator for devices in Waybar output
    #[arg(short, long, default_value = " | ")]
    waybar_separator: String,

    /// Interval in seconds between each loop
    #[arg(short, long, default_value_t = 30)]
    interval: u64,

    /// Output format: short, long, or icons
    #[arg(long, default_value = "long", value_enum)]
    format: OutputFormat,

    /// Unit for temperature (C or F)
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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    process_devices_loop(&args).await?;
    Ok(())
}

// Main processing loop
async fn process_devices_loop(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();
    let mut door_status_map: HashMap<String, bool> = HashMap::new();

    loop {
        let mut outputs = Vec::new();

        for device in &args.devices {
            if let Some(output) = process_device(device, args, &client, &mut door_status_map).await
            {
                outputs.push(output);
            }
        }

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

        thread::sleep(Duration::from_secs(args.interval));
    }
}

// Process a single device
async fn process_device(
    device: &str,
    args: &Args,
    client: &Client,
    door_status_map: &mut HashMap<String, bool>,
) -> Option<Value> {
    let (device_type_str, device_id, device_name) = parse_device_info(device)?;
    let device_status =
        fetch_device_status(client, &args.base_url, device_id, &args.auth_key).await?;

    let device_type = if device_type_str.is_empty() {
        autodetect_device_type(&device_status)?
    } else {
        match_device_type(device_type_str)?
    };

    let mut output = match device_type {
        DeviceType::Temperature => {
            parse_temperature_data(device_status, args.format.clone(), &args.unit)
        }
        DeviceType::Plug => parse_plug_data(device_status, args.format.clone()),
        DeviceType::Door => {
            handle_door_status(
                device_id,
                device_name.clone(),
                &device_status,
                door_status_map,
            )?;
            parse_window_or_door_data(device_status, false, args.format.clone())
        }
        DeviceType::Window => parse_window_or_door_data(device_status, true, args.format.clone()),
    };

    // Add device name to output
    if let Some(name) = device_name {
        output["text"] = serde_json::Value::String(format!(
            "{} ({})",
            output["text"].as_str().unwrap_or_default(),
            name
        ));
        output["tooltip"] = serde_json::Value::String(format!(
            "Device: {}\n{}",
            name,
            output["tooltip"].as_str().unwrap_or_default()
        ));
    }

    Some(output)
}

// Parse device information from input string
fn parse_device_info(device: &str) -> Option<(&str, &str, Option<String>)> {
    let parts: Vec<&str> = device.splitn(3, ':').collect();
    if parts.len() < 2 {
        eprintln!("Invalid device format: {}", device);
        return None;
    }

    let device_type_str = parts[0];
    let device_id = parts[1];
    let device_name = parts.get(2).map(|s| s.to_string());

    Some((device_type_str, device_id, device_name))
}

// Fetch device status from API
async fn fetch_device_status(
    client: &Client,
    base_url: &str,
    device_id: &str,
    auth_key: &str,
) -> Option<Value> {
    let full_url = format!("{}/device/status", base_url);

    let response = client
        .post(&full_url)
        .form(&[("id", device_id), ("auth_key", auth_key)])
        .send()
        .await
        .ok()?;

    let status: ShellyResponse = response.json().await.ok()?;

    if !status.isok {
        if let Some(errors) = status.errors {
            if let Some(error_message) = errors.get("invalid_token") {
                eprintln!(
                    "Error: Invalid token - {}",
                    error_message.as_str().unwrap_or("Unknown error")
                );
            } else {
                eprintln!("Error: API returned an error - {errors}");
            }
        } else {
            eprintln!("Error: Unknown error occurred.");
        }
        return None;
    }

    status.data?.device_status
}

// Match device type from string
fn match_device_type(device_type_str: &str) -> Option<DeviceType> {
    match device_type_str.to_lowercase().as_str() {
        "temperature" => Some(DeviceType::Temperature),
        "plug" => Some(DeviceType::Plug),
        "door" => Some(DeviceType::Door),
        "window" => Some(DeviceType::Window),
        _ => {
            eprintln!(
                "Unsupported device type: '{}'. Supported types are: temperature, plug, door, window.",
                device_type_str
            );
            None
        }
    }
}

// Autodetect device type from JSON
fn autodetect_device_type(json: &Value) -> Option<DeviceType> {
    if json.get("temperature:0").is_some() || json.get("humidity:0").is_some() {
        return Some(DeviceType::Temperature);
    }
    if json.get("switch:0").is_some() {
        return Some(DeviceType::Plug);
    }
    if json.get("window:0").is_some() {
        return Some(DeviceType::Door);
    }
    if json.get("tilt:0").is_some() {
        return Some(DeviceType::Window);
    }
    eprintln!("Unable to autodetect device type.");
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Test: Autodetect Device Type
    #[test]
    fn test_autodetect_device_type() {
        let temp_json = json!({ "temperature:0": { "tC": 22.5 } });
        let plug_json = json!({ "switch:0": { "apower": 50.0 } });
        let door_json = json!({ "window:0": { "open": true } });
        let window_json = json!({ "tilt:0": { "angle": 30 } });
        let unknown_json = json!({});

        assert_eq!(
            autodetect_device_type(&temp_json),
            Some(DeviceType::Temperature)
        );
        assert_eq!(autodetect_device_type(&plug_json), Some(DeviceType::Plug));
        assert_eq!(autodetect_device_type(&door_json), Some(DeviceType::Door));
        assert_eq!(
            autodetect_device_type(&window_json),
            Some(DeviceType::Window)
        );
        assert_eq!(autodetect_device_type(&unknown_json), None);
    }

    // Test: Match Device Type
    #[test]
    fn test_match_device_type() {
        assert_eq!(
            match_device_type("temperature"),
            Some(DeviceType::Temperature)
        );
        assert_eq!(match_device_type("plug"), Some(DeviceType::Plug));
        assert_eq!(match_device_type("door"), Some(DeviceType::Door));
        assert_eq!(match_device_type("window"), Some(DeviceType::Window));
        assert_eq!(match_device_type("unknown"), None);
    }

    // Test: Parse Device Info
    #[test]
    fn test_parse_device_info() {
        let device = "temperature:12345:Living Room";
        let device_with_no_name = "plug:67890";
        let invalid_device = "invalidformat";

        assert_eq!(
            parse_device_info(device),
            Some(("temperature", "12345", Some("Living Room".to_string())))
        );
        assert_eq!(
            parse_device_info(device_with_no_name),
            Some(("plug", "67890", None))
        );
        assert_eq!(parse_device_info(invalid_device), None);
    }

    // Test: Parse Temperature Data
    #[test]
    fn test_parse_temperature_data() {
        let device_status = json!({
            "temperature:0": { "tC": 22.5, "tF": 72.5 },
            "humidity:0": { "rh": 50 },
            "devicepower:0": { "battery": { "percent": 80 } },
            "reporter": { "rssi": -60 }
        });

        let output = parse_temperature_data(device_status.clone(), OutputFormat::Short, "C");
        assert_eq!(output["text"], "T: 22.5°C H: 50%");
        assert_eq!(output["tooltip"], "B: 80% RSSI: -60dBm");

        let output = parse_temperature_data(device_status.clone(), OutputFormat::Long, "F");
        assert_eq!(output["text"], "Temp: 72.5°F Humidity: 50%");
        assert_eq!(output["tooltip"], "Battery: 80% RSSI: -60dBm");

        let output = parse_temperature_data(device_status, OutputFormat::Icons, "C");
        assert_eq!(output["text"], "22.5°C 💧50%");
        assert_eq!(output["tooltip"], "🔋80% 📶-60dBm");
    }

    // Test: Parse Plug Data
    #[test]
    fn test_parse_plug_data() {
        let device_status = json!({
            "switch:0": { "apower": 50.0, "voltage": 230.0, "current": 0.217, "output": true },
            "wifi": { "rssi": -70 }
        });

        let output = parse_plug_data(device_status.clone(), OutputFormat::Short);
        assert_eq!(output["text"], "P: 50.0W V: 230.0V");
        assert_eq!(output["tooltip"], "I: 0.217A RSSI: -70dBm O: ON");

        let output = parse_plug_data(device_status.clone(), OutputFormat::Long);
        assert_eq!(output["text"], "Power: 50.0W Voltage: 230.0V");
        assert_eq!(
            output["tooltip"],
            "Current: 0.217A WiFi RSSI: -70dBm Output: ON"
        );

        let output = parse_plug_data(device_status, OutputFormat::Icons);
        assert_eq!(output["text"], "⚡50.0W 🔌230.0V");
        assert_eq!(output["tooltip"], "🔋0.217A 📶-70dBm 🔆ON");
    }

    // Test: Parse Window/Door Data
    #[test]
    fn test_parse_window_or_door_data() {
        let device_status = json!({
            "window:0": { "open": true },
            "illuminance:0": { "lux": 100 },
            "devicepower:0": { "battery": { "percent": 90 } },
            "reporter": { "rssi": -65 },
            "tilt:0": { "angle": 30 }
        });

        let output = parse_window_or_door_data(device_status.clone(), true, OutputFormat::Short);
        assert_eq!(output["text"], "Open: L: 100, Tilt: 30");
        assert_eq!(output["tooltip"], "B: 90% RSSI: -65dBm");

        let output = parse_window_or_door_data(device_status.clone(), false, OutputFormat::Long);
        assert_eq!(output["text"], "Open, Lux: 100");
        assert_eq!(output["tooltip"], "Battery: 90% RSSI: -65dBm");

        let output = parse_window_or_door_data(device_status, true, OutputFormat::Icons);
        assert_eq!(output["text"], "🟢 🔆100, Tilt: 30");
        assert_eq!(output["tooltip"], "🔋90% 📶-65dBm");
    }

    // Test: Door Status Change Notification
    #[test]
    fn test_handle_door_status() {
        let mut door_status_map = HashMap::new();
        let device_status_open = json!({
            "window:0": { "open": true }
        });
        let device_status_closed = json!({
            "window:0": { "open": false }
        });

        let device_id = "door-12345";
        let device_name = Some("Front Door".to_string());

        // Test status change from None to Open
        let notification = handle_door_status(
            device_id,
            device_name.clone(),
            &device_status_open,
            &mut door_status_map,
        );
        assert!(notification.is_some());
        assert!(door_status_map[&format!("{}:{}", device_id, device_name.clone().unwrap())]);

        // Test status change from Open to Closed
        let notification = handle_door_status(
            device_id,
            device_name.clone(),
            &device_status_closed,
            &mut door_status_map,
        );
        assert!(notification.is_some());
        assert!(!door_status_map[&format!("{}:{}", device_id, device_name.clone().unwrap())]);
    }

    // Test: Fetch Device Status Mock
    #[tokio::test]
    async fn test_fetch_device_status() {
        use httpmock::MockServer;

        let server = MockServer::start_async().await;
        let mock_response = json!({
            "isok": true,
            "data": {
                "device_status": {
                    "temperature:0": { "tC": 22.5, "tF": 72.5 },
                    "humidity:0": { "rh": 50 }
                }
            }
        });

        let mock = server.mock(|when, then| {
            when.method("POST").path("/device/status");
            then.status(200).json_body(mock_response.clone());
        });

        let client = Client::new();
        let response =
            fetch_device_status(&client, &server.base_url(), "12345", "mock-auth-key").await;

        mock.assert();
        assert!(response.is_some());
        assert_eq!(
            response.unwrap()["temperature:0"]["tC"],
            mock_response["data"]["device_status"]["temperature:0"]["tC"]
        );
    }
}
