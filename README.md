# Waybar toy to display Shelly sensor info

## Usage

Get your `base_url` and `auth_key` from https://control.shelly.cloud/#/settings/user
Get devices IDs from each device, in Settings/Device informations.

### Try it

```
$ cargo run -- --devices "temperature:<device_id1>" --devices "door:<device_id2>[:<name2>]" --auth-key <auth_key> --base-url https://shelly-001-eu.shelly.cloud [--format long,short,icons] [--unit C,F]
```

### Waybar integration

$ ~/.config/waybar/config

```json
{
  "custom/shelly": {
    "exec": "shelly-waybar [--interval 30] --devices temperature:12345:Balcony --devices plug:67890 --auth-key <YOUR_AUTH_KEY> --base-url  https://shelly-001-eu.shelly.cloud",
    "return-type": "json",
    "tooltip": true
  }
}
```
