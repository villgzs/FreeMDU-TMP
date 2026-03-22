use freemdu::{Interface};
use freemdu::ws_bridge::WsBridgePort;
use std::{error::Error, thread, time::Duration};
use tokio::time;

// First try valid keys for existing models
const READ_KEYS: [u16; 8] = [
    0x43ea, 0xb4ee, 0x1234, 0x2b67, 0x04d2, 0x2e69, 0x15a8, 0xbde7,
];
const FULL_ACCESS_KEYS: [u16; 8] = [
    0x1f02, 0x4e83, 0x5678, 0x8235, 0x162e, 0x3e3b, 0x703d, 0x902f,
];

const UNLOCK_TIMEOUT: Duration = Duration::from_millis(500);
const ERROR_RETRY_DELAY: Duration = Duration::from_secs(4);
const CHECK_TIMEOUT: Duration = Duration::from_millis(200);

async fn find_read_access_key(intf: &mut Interface<WsBridgePort>) -> Result<u16, Box<dyn Error>> {
    for i in READ_KEYS.iter().copied().chain(0x0000u16..=0xffffu16) {
        println!("Trying read access key: {i:04x}");

        while let Err(err) = time::timeout(UNLOCK_TIMEOUT, async {
            intf.query_software_id().await?;
            intf.unlock_read_access(i).await
        })
        .await
        {
            eprintln!("Error: {err}");
            thread::sleep(ERROR_RETRY_DELAY);
        }

        // Check if read access was successfully unlocked
        if let Ok(Ok(_)) = time::timeout(CHECK_TIMEOUT, intf.read_memory::<u8, _>(0x0000)).await {
            return Ok(i);
        }
    }

    Err("Failed to find read access key".into())
}

async fn find_full_access_key(
    intf: &mut Interface<WsBridgePort>,
    read_key: u16,
) -> Result<u16, Box<dyn Error>> {
    for i in FULL_ACCESS_KEYS
        .iter()
        .copied()
        .chain(0x0000u16..=0xffffu16)
    {
        println!("Trying read & full access keys: {read_key:04x}, {i:04x}");

        while let Err(err) = time::timeout(UNLOCK_TIMEOUT, async {
            intf.query_software_id().await?;
            intf.unlock_read_access(read_key).await?;
            intf.unlock_full_access(i).await
        })
        .await
        {
            eprintln!("Error: {err}");
            thread::sleep(ERROR_RETRY_DELAY);
        }

        // Check if full access was successfully unlocked
        if let Ok(Ok(())) = time::timeout(CHECK_TIMEOUT, intf.halt()).await {
            return Ok(i);
        }
    }


    Err("Failed to find full access key".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:8080/optical".to_string());

    let mut port = freemdu::ws_bridge::WsBridgePort::connect(&url).await?;
    //let dev = freemdu::device::connect(&mut port).await?;

    let mut intf = Interface::new(port);
    let read_key = find_read_access_key(&mut intf).await?;
    let full_key = find_full_access_key(&mut intf, read_key).await?;

    println!("Found keys: {read_key:04x}, {full_key:04x}");

    Ok(())
}
