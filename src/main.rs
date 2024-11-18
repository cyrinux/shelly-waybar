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

    /// Auth key for the Shelly API
    #[arg(short, long)]
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

#[derive(Debug, EnumString, Display)]
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
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("This application is Linux-only.");
        std::process::exit(1);
    }

    let args = Args::parse();
    let client = Client::new();
    let mut door_status_map: HashMap<String, bool> = HashMap::new();

    loop {
        let mut outputs = Vec::new();

        for device in &args.devices {
            // Parse <device_type>:<device_id>:<device_name>
            let parts: Vec<&str> = device.splitn(3, ':').collect();
            if parts.len() < 2 {
                eprintln!("Invalid device format: {device}");
                continue;
            }

            // If multiple devices are specified, ensure `device_name` is provided
            if args.devices.len() > 1 && parts.len() < 3 {
                eprintln!(
                    "Error: Device name is required when multiple devices are specified. Invalid device: {device}"
                );
                std::process::exit(1); // Exit immediately if validation fails
            }

            let device_type: DeviceType = match parts[0].parse() {
                Ok(dt) => dt,
                Err(_) => {
                    let supported_types = vec![
                        DeviceType::Temperature,
                        DeviceType::Plug,
                        DeviceType::Door,
                        DeviceType::Window,
                    ];
                    eprintln!(
                        "Unsupported device type: '{}'. Supported types are: {}",
                        parts[0],
                        supported_types
                            .iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    continue;
                }
            };

            let device_id = parts[1];
            let device_name = parts.get(2).map(|s| s.to_string()); // Optional device name

            // Construct the full URL
            let full_url = format!("{}/device/status", args.base_url);

            // Fetch data for the device
            let response = client
                .post(&full_url)
                .form(&[("id", device_id), ("auth_key", &args.auth_key)])
                .send()
                .await?;

            let status: ShellyResponse = response.json().await?;

            // Check if the response indicates an error
            if !status.isok {
                if let Some(errors) = &status.errors {
                    if let Some(error_message) = errors.get("invalid_token") {
                        eprintln!(
                            "Error: Invalid token - {}",
                            error_message.as_str().unwrap_or("Unknown error")
                        );
                    } else {
                        eprintln!("Error: API returned an error - {}", errors.to_string());
                    }
                } else {
                    eprintln!("Error: Unknown error occurred.");
                }
                continue;
            }

            if let Some(data) = status.data {
                if let Some(device_status) = data.device_status {
                    let mut output = match device_type {
                        DeviceType::Temperature => {
                            parse_temperature_data(device_status, args.format.clone(), &args.unit)
                        }
                        DeviceType::Plug => parse_plug_data(device_status, args.format.clone()),
                        DeviceType::Door => {
                            let is_open =
                                device_status["window:0"]["open"].as_bool().unwrap_or(false);

                            // Check for door status changes and notify
                            let status_key = format!(
                                "{}:{}",
                                device_id,
                                device_name.clone().unwrap_or_default()
                            );
                            if let Some(prev_status) = door_status_map.get(&status_key) {
                                if *prev_status != is_open {
                                    let state = if is_open { "Open" } else { "Closed" };
                                    let name = device_name
                                        .clone()
                                        .unwrap_or_else(|| "Unnamed Door".to_string());

                                    Notification::new()
                                        .summary(&format!("Door Status Changed: {name}"))
                                        .body(&format!("The door is now {state}"))
                                        .show()?;
                                }
                            }

                            // Update the door status map
                            door_status_map.insert(status_key, is_open);

                            parse_window_or_door_data(device_status, false, args.format.clone())
                        }
                        DeviceType::Window => {
                            parse_window_or_door_data(device_status, true, args.format.clone())
                        }
                    };

                    // Add device name to the output if available
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

                    outputs.push(output);
                }
            }
        }

        // Merge all device outputs into a single object
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

            println!("{}", merged_output.to_string());
        }

        // Sleep for the specified interval
        thread::sleep(Duration::from_secs(args.interval));
    }
}

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
