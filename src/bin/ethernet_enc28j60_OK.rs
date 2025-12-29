#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_net::tcp::TcpSocket;
use embassy_net::StackResources;
use embassy_net_enc28j60::Enc28j60;
use embassy_stm32::spi::Spi;
use embassy_stm32::spi::Config;
use embassy_stm32::time::Hertz;
//use embassy_stm32::usart::{Config, Uart};
use embassy_stm32::{bind_interrupts, peripherals, usart};
use embassy_stm32::gpio::{Output, Level, Speed};
use embassy_time::Delay;
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_io_async::Write;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    USART2 => usart::InterruptHandler<peripherals::USART2>;
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

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());

 //   let config = usart::Config::default();
   // let mut usart = Uart::new(p.USART2, p.PA2, p.PA3, Irqs, p.DMA1_CH5, p.DMA1_CH1, config).unwrap();
	
    let mut spi_config = Config::default();
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

    unwrap!(spawner.spawn(net_task(runner)));

    // And now we can use it!

    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];
    let mut buf = [0; 4096];

    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        info!("Listening on TCP:1234...");
        if let Err(e) = socket.accept(1234).await {
            warn!("accept error: {:?}", e);
            continue;
        }

        info!("Received connection from {:?}", socket.remote_endpoint());

        loop {
            let n = match socket.read(&mut buf).await {
                Ok(0) => {
                    warn!("read EOF");
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    warn!("read error: {:?}", e);
                    break;
                }
            };

            info!("rxd {:02x}", &buf[..n]);

            match socket.write_all(&buf[..n]).await {
                Ok(()) => {}
                Err(e) => {
                    warn!("write error: {:?}", e);
                    break;
                }
            };
        }
    }
}
