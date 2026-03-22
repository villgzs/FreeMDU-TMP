//! Live read of **`0x1d6f`** on software ID **2465**, decoding with the **id1998 `ProgramOption`**
//! hypothesis (`raw ^ 0x0040`).
//!
//! ```sh
//! cargo run -p freemdu --example experimental_program_options_ws \
//!   --features bridge-ws-client,experimental-decode-program-options -- \
//!   ws://192.168.1.210:8080/optical
//! ```

use std::error::Error;

use freemdu::device::Device;
use freemdu::device::id2465::{
    decode_program_options_id1998_hypothesis, decode_program_options_id1998_hypothesis_truncated,
    WashingMachine,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:8080/optical".to_string());

    let mut port = freemdu::ws_bridge::WsBridgePort::connect(&url).await?;
    let mut m = WashingMachine::connect(&mut port).await?;

    println!("{} — software ID {}\n", m.kind(), m.software_id());

    let raw = m.query_program_options_raw().await?;
    let spin = m.query_program_spin_speed().await?;
    let valid = m.query_program_memory_valid().await?;

    println!("program_memory_valid     {valid}");
    println!("program_spin_speed_rpm   {spin}");
    println!("program_options_raw      {raw} (0x{raw:04x})");

    let xor40 = raw ^ 0x0040;
    println!("raw ^ 0x0040             0x{xor40:04x}");

    match decode_program_options_id1998_hypothesis(raw) {
        Some(opts) => println!("id1998 hypothesis (strict):   {opts}"),
        None => println!("id1998 hypothesis (strict):   <none — reserved or unknown bits>"),
    }
    let trunc = decode_program_options_id1998_hypothesis_truncated(raw);
    println!(
        "id1998 hypothesis (truncated): {}",
        if trunc.is_empty() {
            "<empty>".into()
        } else {
            trunc.to_string()
        }
    );

    println!(
        "\nDevice property string: {}",
        m.query_program_options_id1998_hypothesis_display().await?
    );

    Ok(())
}
