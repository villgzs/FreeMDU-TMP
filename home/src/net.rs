//! Shared Wi‑Fi and TCP/IP stack setup for firmware binaries that need networking.

extern crate alloc;

use alloc::string::String;
use anyhow::Result;
use core::fmt::Write;
use embassy_net::{DhcpConfig, Runner, Stack, StackResources};
use embassy_time::Duration;
use esp_hal::{peripherals::WIFI, rng::Rng};
use esp_radio::{
    Controller,
    wifi::{
        self, ClientConfig, CountryInfo, ModeConfig, OperatingClass, WifiController, WifiDevice,
        WifiEvent,
    },
};
use log::{error, info};
use static_cell::StaticCell;

/// Delay between Wi‑Fi reconnection attempts.
pub const WIFI_RETRY_DELAY: Duration = Duration::from_secs(5);

#[embassy_executor::task]
pub async fn network_stack_task(mut runner: Runner<'static, WifiDevice<'static>>) -> ! {
    runner.run().await;
}

#[embassy_executor::task]
pub async fn wifi_connect_task(mut controller: WifiController<'static>) -> ! {
    loop {
        match controller.connect_async().await {
            Ok(()) => {
                info!("Wi-Fi connected");
                controller.wait_for_event(WifiEvent::StaDisconnected).await;
                info!("Wi-Fi disconnected");
            }
            Err(err) => {
                error!("Failed to connect to Wi-Fi: {err:?}");
                embassy_time::Timer::after(WIFI_RETRY_DELAY).await;
            }
        }
    }
}

pub fn init_wifi(wifi: WIFI<'static>) -> Result<(WifiController<'static>, WifiDevice<'static>)> {
    static CONTROLLER: StaticCell<Controller<'_>> = StaticCell::new();

    let controller = CONTROLLER.init(
        esp_radio::init().map_err(|err| anyhow::anyhow!("Failed to initialize radio: {err:?}"))?,
    );
    let (mut controller, intfs) = wifi::new(
        controller,
        wifi,
        wifi::Config::default().with_country_code(
            CountryInfo::from(*b"01").with_operating_class(OperatingClass::Indoors),
        ),
    )
    .map_err(|err| anyhow::anyhow!("Failed to create Wi-Fi controller: {err:?}"))?;

    controller
        .set_config(&ModeConfig::Client(
            ClientConfig::default()
                .with_ssid(env!("WIFI_SSID").into())
                .with_password(env!("WIFI_PASSWORD").into()),
        ))
        .map_err(|err| anyhow::anyhow!("Failed to set Wi-Fi configuration: {err:?}"))?;
    controller
        .start()
        .map_err(|err| anyhow::anyhow!("Failed to start Wi-Fi controller: {err:?}"))?;

    Ok((controller, intfs.sta))
}

pub fn hostname_from_wifi(dev: &WifiDevice<'_>) -> Result<String> {
    let mut hostname = String::with_capacity(32);

    write!(&mut hostname, "freemdu_home_")?;

    for byte in dev.mac_address() {
        write!(&mut hostname, "{byte:02x}")?;
    }

    Ok(hostname)
}

pub fn init_network(
    dev: WifiDevice<'static>,
    hostname: &str,
) -> Result<(Stack<'static>, Runner<'static, WifiDevice<'static>>)> {
    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();

    let resources = RESOURCES.init(StackResources::new());
    let rng = Rng::new();
    let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());
    let mut cfg = DhcpConfig::default();

    cfg.hostname = Some(
        hostname
            .try_into()
            .map_err(|err| anyhow::anyhow!("Failed to set DHCP hostname: {err:?}"))?,
    );

    Ok(embassy_net::new(
        dev,
        embassy_net::Config::dhcpv4(cfg),
        resources,
        seed,
    ))
}
