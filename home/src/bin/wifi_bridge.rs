#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::gpio::Output;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::timer::timg::TimerGroup;
use esp_println::logger;
use freemdu_home::{net, new_optical_port, new_status_led, OpticalPort};
use log::{info, warn};
use picoserve::{
    io::{Read as IoRead, Write as IoWrite},
    response::ws::{Message, WebSocketCallback, WebSocketUpgrade},
    routing::get,
    Router, Server,
};
use static_cell::StaticCell;

const OPT_BUF_SIZE: usize = 32;
const WS_BUF_SIZE: usize = 512;
const WS_POLL_TICK: Duration = Duration::from_millis(4);
const LED_MIN_ON_DURATION: Duration = Duration::from_millis(10);

static LED_SIGNAL: embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();

static OPTICAL_CELL: StaticCell<Mutex<CriticalSectionRawMutex, OpticalPort<'static>>> =
    StaticCell::new();

static mut OPTICAL_MTX: Option<&'static Mutex<CriticalSectionRawMutex, OpticalPort<'static>>> = None;

esp_bootloader_esp_idf::esp_app_desc!();

struct BridgeCb;

fn optical_mtx() -> &'static Mutex<CriticalSectionRawMutex, OpticalPort<'static>> {
    // SAFETY: set once in `main` before the HTTP server accepts connections.
    unsafe { OPTICAL_MTX.expect("optical mutex not initialized") }
}

impl WebSocketCallback for BridgeCb {
    async fn run<R, W>(
        self,
        mut rx: picoserve::response::ws::SocketRx<R>,
        mut tx: picoserve::response::ws::SocketTx<W>,
    ) -> Result<(), W::Error>
    where
        R: IoRead,
        W: IoWrite<Error = R::Error>,
    {
        Self::bridge_loop(&mut rx, &mut tx).await
    }
}

impl BridgeCb {
    async fn bridge_loop<R, W>(
        rx: &mut picoserve::response::ws::SocketRx<R>,
        tx: &mut picoserve::response::ws::SocketTx<W>,
    ) -> Result<(), W::Error>
    where
        R: IoRead,
        W: IoWrite<Error = R::Error>,
    {
        let mut opt_buf = [0x00; OPT_BUF_SIZE];
        let mut ws_buf = [0x00; WS_BUF_SIZE];
        let optical = optical_mtx();

        loop {
            use picoserve::futures::Either;

            match rx
                .next_message(&mut ws_buf, Timer::after(WS_POLL_TICK))
                .await
            {
                Ok(Either::First(Ok(Message::Binary(data)))) => {
                    LED_SIGNAL.signal(());
                    let mut g = optical.lock().await;
                    if embedded_io_async::Write::write_all(&mut *g, data)
                        .await
                        .is_err()
                    {
                        break;
                    }
                    drop(g);
                    // Forward any bytes already in the RX FIFO (replies can arrive before the next poll).
                    loop {
                        let n = {
                            let mut g = optical.lock().await;
                            match g.read_buffered(&mut opt_buf) {
                                Ok(n) => n,
                                Err(e) => {
                                    warn!("UART read_buffered after TX: {e:?}");
                                    break;
                                }
                            }
                        };
                        if n == 0 {
                            break;
                        }
                        tx.send_binary(&opt_buf[..n]).await?;
                    }
                }
                Ok(Either::First(Ok(Message::Ping(ping)))) => {
                    tx.send_pong(ping).await?;
                }
                Ok(Either::First(Ok(Message::Pong(_)))) => {}
                Ok(Either::First(Ok(Message::Text(_)))) => {}
                Ok(Either::First(Ok(Message::Close(_)))) => break,
                Ok(Either::First(Err(err))) => {
                    warn!("WebSocket message error: {:?}", err);
                    break;
                }
                Ok(Either::Second(())) => {
                    // Must not block on UART here: with an empty FIFO, `read` would wait forever and
                    // we'd never read the next WebSocket frame from the host (USB bridge uses `select`).
                    let n = {
                        let mut g = optical.lock().await;
                        match g.read_buffered(&mut opt_buf) {
                            Ok(n) => n,
                            Err(e) => {
                                warn!("UART read_buffered: {e:?}");
                                0
                            }
                        }
                    };
                    if n > 0 {
                        tx.send_binary(&opt_buf[..n]).await?;
                    }
                }
                Err(err) => {
                    warn!("WebSocket read I/O error: {:?}", err);
                    break;
                }
            }
        }

        Ok(())
    }
}

#[embassy_executor::task]
async fn led_task(mut led: Output<'static>) -> ! {
    loop {
        match embassy_time::with_timeout(LED_MIN_ON_DURATION, LED_SIGNAL.wait()).await {
            Ok(()) => led.set_low(),
            Err(embassy_time::TimeoutError) => led.set_high(),
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default());

    esp_alloc::heap_allocator!(size: 128 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);

    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    let optical = new_optical_port(peripherals.UART1).unwrap();
    let led = new_status_led();

    let mtx: &'static Mutex<CriticalSectionRawMutex, OpticalPort<'static>> =
        OPTICAL_CELL.init(Mutex::new(optical));
    unsafe {
        OPTICAL_MTX = Some(mtx);
    }

    let router = Box::leak(Box::new(
        Router::new()
            .route(
                "/",
                get(|| async {
                    "FreeMDU WiFi optical bridge\r\n\r\nConnect with a WebSocket client to /optical; use binary frames for raw optical bytes.\r\n"
                }),
            )
            .route("/optical", get(|upgrade: WebSocketUpgrade| async move {
                upgrade.on_upgrade(BridgeCb)
            })),
    ));

    let port: u16 = freemdu_home::num_from_env!("BRIDGE_HTTP_PORT", u16);

    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Some(Duration::from_secs(30)),
        persistent_start_read_request: Some(Duration::from_secs(300)),
        read_request: Some(Duration::from_secs(30)),
        write: Some(Duration::from_secs(60)),
    })
    .close_connection_after_response();

    let mut rx_buffer = [0u8; 4096];
    let mut tx_buffer = [0u8; 4096];
    let mut http_buffer = [0u8; 2048];

    let (wifi_controller, wifi_dev) = net::init_wifi(peripherals.WIFI).unwrap();
    let hostname = net::hostname_from_wifi(&wifi_dev).unwrap();
    let (net_stack, net_runner) = net::init_network(wifi_dev, &hostname).unwrap();

    spawner.spawn(led_task(led)).unwrap();
    spawner.spawn(net::network_stack_task(net_runner)).unwrap();
    spawner.spawn(net::wifi_connect_task(wifi_controller)).unwrap();

    net_stack.wait_config_up().await;
    match net_stack.config_v4() {
        Some(cfg) => info!("Wi-Fi IPv4 address {}", cfg.address.address()),
        None => warn!("IPv4 configuration missing after DHCP"),
    }
    info!("HTTP optical bridge on TCP {port} (hostname {hostname})");

    loop {
        Server::new(router, &config, &mut http_buffer)
            .listen_and_serve("wifi", net_stack, port, &mut rx_buffer, &mut tx_buffer)
            .await;
    }
}
