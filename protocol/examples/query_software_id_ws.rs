//! Query software ID and every [`Device`](freemdu::device::Device) property over the Wi‑Fi WebSocket
//! optical bridge.
//!
//! Usage:
//! ```sh
//! cargo run --example query_software_id_ws --features bridge-ws-client -- ws://192.168.1.210:8080/optical
//! ```

use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:8080/optical".to_string());

    let mut port = freemdu::ws_bridge::WsBridgePort::connect(&url).await?;
    let mut dev = freemdu::device::connect(&mut port).await?;

    println!(
        "{} — software ID {}\n",
        dev.kind(),
        dev.software_id()
    );

    for prop in dev.properties() {
        match dev.query_property(prop).await {
            Ok(val) => println!("  {:<28} {}", prop.name, format_value(&val)),
            Err(e) => println!("  {:<28} <error: {e}>", prop.name),
        }
    }

    Ok(())
}

fn format_value(v: &freemdu::device::Value) -> String {
    use freemdu::device::Value;
    match v {
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Sensor(a, b) => format!("{a} / {b}"),
        Value::String(s) => s.clone(),
        Value::Duration(d) => format!("{d:?}"),
        Value::Date(d) => format!("{}-{:02}-{:02}", d.year, d.month, d.day),
        Value::Fault(f) => format!("{f:?}"),
    }
}
