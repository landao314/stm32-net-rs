#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_sync::channel::Channel;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_futures::select::{select, Either};
use embassy_net::tcp::TcpSocket;
use embassy_net::StackResources;
use embassy_net_enc28j60::Enc28j60;
use embassy_stm32::spi::Spi;
use embassy_stm32::time::Hertz;
use embassy_stm32::usart::Uart;
use embassy_stm32::mode::Async;
use embassy_stm32::{bind_interrupts, peripherals, usart};
use embassy_stm32::gpio::{Output, Level, Speed};
use embassy_time::{Delay, Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_io_async::Write;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

const BUF_SIZE: usize = 2048;

static Uart_to_eth: Channel<CriticalSectionRawMutex, &'static [u8], 2> = Channel::new();
static Eth_to_uart: Channel<CriticalSectionRawMutex, &'static [u8], 2> = Channel::new();

static Buf_eth: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();
static Buf_uart: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();

bind_interrupts!(struct Irqs {
    USART1 => usart::InterruptHandler<peripherals::USART1>;
});


#[embassy_executor::task]
async fn net_task(
    mut runner: embassy_net::Runner<
        'static,
        Enc28j60<ExclusiveDevice<embassy_stm32::spi::Spi<'static,embassy_stm32::mode::Async>,Output<'static>,Delay>,Output<'static>>,
    >,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn uart_process(mut uart:Uart<'static, Async>, mut buf: &'static mut [u8; BUF_SIZE]) {
    loop{
        match select(
            uart.read_until_idle(buf),
            Eth_to_uart.receive(),
        ).await {
            Either::First(result) => {
                if let Ok(n) = result {
                        Uart_to_eth.send(&buf[..n]).await;
                    }
                }
            Either::Second(data) => {
                if let Err(e) = uart.write_all(data).await {
                    warn!("uart write error: {:?}",e);
                }
            }
        }
    }
}
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

    let config = usart::Config::default();
    let usart = Uart::new(p.USART1, p.PA10, p.PA9, Irqs, p.DMA1_CH4, p.DMA1_CH5, config).unwrap();
    let (mut tx, mut rx) = usart.split();
	
    let mut spi_config = embassy_stm32::spi::Config::default();
    spi_config.frequency = Hertz(1_000_000);  // 1 MHz

    let spi = Spi::new(
        p.SPI1,
        p.PA5,  // SCK
        p.PA7,  // MOSI
        p.PA6,  // MISO
        p.DMA1_CH3,
        p.DMA1_CH2,
        spi_config,
    );

    let cs = Output::new(p.PA4, Level::High, Speed::VeryHigh);
    
    let spi_dev = ExclusiveDevice::new(spi, cs, Delay);
    
    let eth_rst = p.PA3;
    
    let rst = Output::new(eth_rst, Level::High, Speed::VeryHigh);
    let mac_addr = [2, 3, 4, 5, 6, 7];
    let device = Enc28j60::new(spi_dev, Some(rst), mac_addr);

    let config = embassy_net::Config::dhcpv4(Default::default());
    // let config = embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
    //    address: Ipv4Cidr::new(Ipv4Address::new(10, 42, 0, 61), 24),
    //    dns_servers: Vec::new(),
    //    gateway: Some(Ipv4Address::new(10, 42, 0, 1)),
    // });

    // Generate random seed
    let seed = [0,4,87,87,45,45,123,233];
    let seed = u64::from_le_bytes(seed);

    // Init network stack
    static RESOURCES: StaticCell<StackResources<2>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(device, config, RESOURCES.init(StackResources::new()), seed);

    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    let ebuf = Buf_eth.init([0; BUF_SIZE]);
    let ubuf = Buf_uart.init([0; BUF_SIZE]);

    unwrap!(spawner.spawn(net_task(runner)));
    unwrap!(spawner.spawn(uart_rx_process(rx,ubuf)));  
    unwrap!(spawner.spawn(uart_tx_process(tx,ubuf)));  

    // And now we can use it!



    'master:loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        info!("Listening on TCP:1234...");
        if let Err(e) = socket.accept(1234).await {
            warn!("accept error: {:?}", e);
            continue;
        }
        info!("Received connection from {:?}", socket.remote_endpoint());

        loop {
            match select(socket.read(ebuf), Uart_to_eth.receive()).await {
                Either::First(result) => {
                    match result {
                        Ok(0) => {
                            warn!("read EOF");
                            break 'master;
                        },
                        Ok(n) => {
                            info!("get eth data:{:?}",n);
                            let msg = &ebuf[..n];
                            Eth_to_uart.send(msg).await;
                        },
                        Err(e) => {
                            warn!("read error: {:?}", e);
                        }
                    }
                }
                Either::Second(data) => {
                    if let Err(e) = socket.write_all(&data).await {
                        warn!("eth write error: {:?}",e);
                    }
                }
            }
            Timer::after(Duration::from_millis(10)).await;
        }
    }
}
