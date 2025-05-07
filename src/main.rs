#![no_std]
#![no_main]
mod dev_att;

use core::cell::RefCell;
use core::fmt::Write;
use core::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use core::pin::pin;
use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_futures::select::{select, select3};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_time::Duration;
use embassy_time::Timer;
use embedded_storage::{ReadStorage, Storage};
use esp_backtrace as _;
use esp_hal::gpio::Level;
use esp_hal::gpio::Output;
use esp_hal::rng::Rng;
use esp_hal::timer::systimer::SystemTimer;
use esp_ieee802154::Ieee802154;
use esp_println::println;
use openthread::esp::EspRadio;
use openthread::BytesFmt;
use openthread::OtRngCore;
use openthread::SrpConf;
use openthread::{
    OpenThread, OtResources, OtSrpResources, OtUdpResources, SimpleRamSettings, UdpSocket,
};
use rs_matter::core::Matter;
use rs_matter::data_model::cluster_basic_information::BasicInfoConfig;
use rs_matter::data_model::cluster_on_off;
use rs_matter::data_model::core::IMBuffer;
use rs_matter::data_model::device_types::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter::data_model::objects::{
    Dataver, Endpoint, HandlerCompat, Metadata, Node, NonBlockingHandler,
};
use rs_matter::data_model::root_endpoint;
use rs_matter::data_model::subscriptions::Subscriptions;
use rs_matter::data_model::system_model::descriptor;
use rs_matter::error::Error;
use rs_matter::error::ErrorCode;
use rs_matter::mdns::MdnsService;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::respond::DefaultResponder;
use rs_matter::transport::network::Address;
use rs_matter::transport::network::{NetworkReceive, NetworkSend};
use rs_matter::utils::init::InitMaybeUninit;
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::storage::pooled::PooledBuffers;
use rs_matter::BasicCommData;
use rs_matter::MATTER_PORT;
use static_cell::StaticCell;

use tinyrlibc as _;

use crate::dev_att::HardCodedDevAtt;

static ALLOCATOR: esp_alloc::EspHeap = esp_alloc::EspHeap::empty();

const BOUND_PORT: u16 = 1212;

const UDP_SOCKETS_BUF: usize = 1280;
const UDP_MAX_SOCKETS: usize = 2;

const SRP_SERVICE_BUF: usize = 300;
const SRP_MAX_SERVICES: usize = 2;

const THREAD_DATASET: &str = if let Some(dataset) = option_env!("THREAD_DATASET") {
    dataset
} else {
    "0e080000000000010000000300000b35060004001fffe002083a90e3a319a904940708fd1fa298dbd1e3290510fe0458f7db96354eaa6041b880ea9c0f030f4f70656e5468726561642d35386431010258d10410888f813c61972446ab616ee3c556a5910c0402a0f7f8"
};

static DEV_DET: BasicInfoConfig = BasicInfoConfig {
    vid: 0xFFF1,
    pid: 0x8001,
    hw_ver: 2,
    sw_ver: 1,
    sw_ver_str: "1",
    serial_no: "aabbccdd",
    device_name: "OnOff Light",
    product_name: "Light123",
    vendor_name: "Vendor PQR",
    sai: None,
    sii: None,
};

static DEV_COMM: BasicCommData = BasicCommData {
    password: 20202021,
    discriminator: 3840,
};

static DEV_ATT: HardCodedDevAtt = HardCodedDevAtt::new();

static MATTER: StaticCell<Matter> = StaticCell::new();

static BUFFERS: StaticCell<PooledBuffers<10, NoopRawMutex, IMBuffer>> = StaticCell::new();

static SUBSCRIPTIONS: StaticCell<Subscriptions<3>> = StaticCell::new();

static RNG: Mutex<RefCell<Option<Rng>>> = Mutex::new(RefCell::new(None));

macro_rules! mk_static {
    ($t:ty) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit();
        x
    }};
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) -> ! {
    println!(
        "Matter memory: Matter (BSS)={}B, IM Buffers (BSS)={}B, Subscriptions (BSS)={}B",
        core::mem::size_of::<Matter>(),
        core::mem::size_of::<PooledBuffers<10, NoopRawMutex, IMBuffer>>(),
        core::mem::size_of::<Subscriptions<3>>(),
    );

    let peripherals = esp_hal::init(esp_hal::Config::default());

    let rng = mk_static!(Rng, Rng::new(peripherals.RNG));

    critical_section::with(|cs| RNG.borrow_ref_mut(cs).replace(*rng));
    let matter = MATTER.uninit().init_with(Matter::init(
        &DEV_DET,
        DEV_COMM,
        &DEV_ATT,
        // NOTE:
        // For `no_std` environments, provide your own epoch and rand functions here
        MdnsService::Builtin,
        || {
            let millis = esp_hal::time::now().duration_since_epoch().to_millis();
            core::time::Duration::from_millis(millis)
        },
        |b: &mut [u8]| {
            critical_section::with(|cs| {
                let mut rng = RNG.borrow(cs).borrow_mut();
                rng.as_mut().unwrap().read(b);
            });
        },
        MATTER_PORT,
    ));

    matter.initialize_transport_buffers().unwrap();

    let buffers = BUFFERS.uninit().init_with(PooledBuffers::init(0));

    println!("IM buffers initialized");

    let on_off = cluster_on_off::OnOffCluster::new(Dataver::new_rand(matter.rand()));

    let subscriptions = SUBSCRIPTIONS.uninit().init_with(Subscriptions::init());

    // Assemble our Data Model handler by composing the predefined Root Endpoint handler with our custom On/Off clusters
    let dm_handler = HandlerCompat(dm_handler(&matter, &on_off));

    // Create a default responder capable of handling up to 3 subscriptions
    // All other subscription requests will be turned down with "resource exhausted"
    let responder = DefaultResponder::new(&matter, buffers, &subscriptions, dm_handler);
    println!(
        "Responder memory: Responder (stack)={}B, Runner fut (stack)={}B",
        core::mem::size_of_val(&responder),
        core::mem::size_of_val(&responder.run::<4, 4>())
    );

    // Run the responder with up to 4 handlers (i.e. 4 exchanges can be handled simultaneously)
    // Clients trying to open more exchanges than the ones currently running will get "I'm busy, please try again later"
    let mut respond = pin!(responder.run::<4, 4>());

    // This is a sample code that simulates state changes triggered by the HAL
    // Changes will be properly communicated to the Matter controllers and other Matter apps (i.e. Google Home, Alexa), thanks to subscriptions
    // Garther the GPIO pin for the red LED (GPIO32)
    let mut led = Output::new(peripherals.GPIO3, Level::Low);
    let mut device = pin!(async {
        loop {
            Timer::after(Duration::from_millis(50)).await;
            led.set_level(if on_off.get() {
                Level::High
            } else {
                Level::Low
            });
        }
    });

    esp_hal_embassy::init(SystemTimer::new(peripherals.SYSTIMER).alarm0);

    let mut ieee_eui64 = [0; 8];
    rng.fill_bytes(&mut ieee_eui64);

    let random_srp_suffix = rng.next_u32();

    let ot_resources = mk_static!(OtResources, OtResources::new());
    let ot_udp_resources =
        mk_static!(OtUdpResources<UDP_MAX_SOCKETS, UDP_SOCKETS_BUF>, OtUdpResources::new());
    let ot_srp_resources =
        mk_static!(OtSrpResources<SRP_MAX_SERVICES, SRP_SERVICE_BUF>, OtSrpResources::new());
    let ot_settings_buf = mk_static!([u8; 1024], [0; 1024]);

    let ot_settings = mk_static!(SimpleRamSettings, SimpleRamSettings::new(ot_settings_buf));

    let ot = OpenThread::new_with_udp_srp(
        ieee_eui64,
        rng,
        ot_settings,
        ot_resources,
        ot_udp_resources,
        ot_srp_resources,
    )
    .unwrap();

    spawner
        .spawn(run_ot(
            ot.clone(),
            EspRadio::new(Ieee802154::new(
                peripherals.IEEE802154,
                peripherals.RADIO_CLK,
            )),
        ))
        .unwrap();

    spawner.spawn(run_ot_ip_info(ot.clone())).unwrap();

    println!("Dataset: {}", THREAD_DATASET);

    ot.set_active_dataset_tlv_hexstr(THREAD_DATASET).unwrap();
    ot.enable_ipv6(true).unwrap();
    ot.enable_thread(true).unwrap();

    let mut hostname = heapless::String::<32>::new();
    write!(hostname, "srp-example-{random_srp_suffix:04x}").unwrap();

    let _ = ot.srp_remove_all(false);

    while !ot.srp_is_empty().unwrap() {
        println!("Waiting for SRP records to be removed...");
        ot.wait_changed().await;
    }

    ot.srp_set_conf(&SrpConf {
        host_name: hostname.as_str(),
        ..SrpConf::new()
    })
    .unwrap();

    let mut servicename = heapless::String::<32>::new();
    write!(servicename, "srp{random_srp_suffix:04x}").unwrap();

    // NOTE: To get the host registered, we need to add at least one service
    ot.srp_add_service(&openthread::SrpService {
        name: "_foo._tcp",
        instance_name: servicename.as_str(),
        port: 777,
        subtype_labels: ["foo"].into_iter(),
        txt_entries: [("a", "b".as_bytes())].into_iter(),
        priority: 0,
        weight: 0,
        lease_secs: 0,
        key_lease_secs: 0,
    })
    .unwrap();

    let socket = UdpSocket::bind(
        ot,
        &SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, BOUND_PORT, 0, 0),
    )
    .unwrap();

    println!(
        "Opened socket on port {} and waiting for packets...",
        BOUND_PORT
    );

    println!(
        "Transport memory: Transport fut (stack)={}B",
        size_of_val(&matter.run(
            MyUdpSocket(&socket),
            MyUdpSocket(&socket),
            DiscoveryCapabilities::IP
        )),
    );
    let mut transport = pin!(matter.run(
        MyUdpSocket(&socket),
        MyUdpSocket(&socket),
        DiscoveryCapabilities::IP
    ));

    // Load data.
    let mut storage = esp_storage::FlashStorage::new();
    {
        let mut psm_buffer = [0; 4100];
        storage.read(0x9000, &mut psm_buffer).unwrap();
        if psm_buffer[0] == 42 && psm_buffer[1] == 0 && psm_buffer[2] == 42 && psm_buffer[3] == 0 {
            // Data is valid, initialize it.
            println!("Loading fabrics");
            matter.load_fabrics(&psm_buffer[4..]).unwrap();
        } else {
            println!("No valid data found, starting from scratch");
        }
    }
    let mut persist = pin!(async {
        loop {
            matter.wait_fabrics_changed().await;
            if matter.fabrics_changed() {
                println!("Storing fabrics");
                let mut psm_buffer = [0; 4100];
                psm_buffer[0] = 42;
                psm_buffer[1] = 0;
                psm_buffer[2] = 42;
                psm_buffer[3] = 0;
                matter.store_fabrics(&mut psm_buffer[4..]).unwrap();
                storage.write(0x9000, &psm_buffer).unwrap();
            }
        }
    });

    let buf: &mut [u8] = unsafe { mk_static!([u8; UDP_SOCKETS_BUF]).assume_init_mut() };

    loop {
        let (len, local, remote) = socket.recv(buf).await.unwrap();

        println!("Got {} from {} on {}", BytesFmt(&buf[..len]), remote, local);

        socket.send(b"Hello", Some(&local), &remote).await.unwrap();
        println!("Sent `b\"Hello\"`");

        select3(
            &mut transport,
            &mut persist,
            select(&mut respond, &mut device).coalesce(),
        )
        .await;
    }
}

fn _analog_to_lux(analog_value: f64) -> f64 {
    const ADC_VOLTAGE_RANGE: f64 = 3.3;
    const ADC_MAX: f64 = 4095.;

    let volts = analog_value * ADC_VOLTAGE_RANGE / ADC_MAX;
    let amps = volts / 10_000.0; // resistor has 10k Ohm
    let microamps = amps * 1_000_000.;
    microamps * 2.0
}

const NODE: Node<'static> = Node {
    id: 0,
    endpoints: &[
        root_endpoint::endpoint(0, root_endpoint::OperNwType::Ethernet),
        Endpoint {
            id: 1,
            device_types: &[DEV_TYPE_ON_OFF_LIGHT],
            clusters: &[descriptor::CLUSTER, cluster_on_off::CLUSTER],
        },
    ],
};

fn dm_handler<'a>(
    matter: &'a Matter<'a>,
    on_off: &'a cluster_on_off::OnOffCluster,
) -> impl Metadata + NonBlockingHandler + 'a {
    (
        NODE,
        root_endpoint::eth_handler(0, matter.rand())
            .chain(
                1,
                descriptor::ID,
                descriptor::DescriptorCluster::new(Dataver::new_rand(matter.rand())),
            )
            .chain(1, cluster_on_off::ID, on_off),
    )
}

#[embassy_executor::task]
async fn run_ot(ot: OpenThread<'static>, radio: EspRadio<'static>) -> ! {
    ot.run(radio).await
}

#[embassy_executor::task]
async fn run_ot_ip_info(ot: OpenThread<'static>) -> ! {
    let mut cur_addrs = heapless::Vec::<(Ipv6Addr, u8), 4>::new();

    loop {
        let mut addrs = heapless::Vec::<(Ipv6Addr, u8), 4>::new();
        ot.ipv6_addrs(|addr| {
            if let Some(addr) = addr {
                let _ = addrs.push(addr);
            }

            Ok(())
        })
        .unwrap();

        if cur_addrs != addrs {
            println!("Got new IPv6 address(es) from OpenThread: {:?}", addrs);

            cur_addrs = addrs;

            println!("Waiting for OpenThread changes signal...");
        }

        ot.wait_changed().await;
    }
}

struct MyUdpSocket<'a, 'b>(&'a UdpSocket<'b>);

impl NetworkSend for MyUdpSocket<'_, '_> {
    async fn send_to(&mut self, data: &[u8], addr: Address) -> Result<(), Error> {
        let addr = match addr {
            Address::Udp(socket_addr) => match socket_addr {
                SocketAddr::V4(_) => todo!(),
                SocketAddr::V6(socket_addr_v6) => socket_addr_v6,
            },
            Address::Tcp(_) => todo!(),
            Address::Btp(_) => todo!(),
        };

        self.0
            .send(data, None, &addr)
            .await
            .map_err(|_| Error::new(ErrorCode::NoEndpoint))
    }
}

impl NetworkReceive for MyUdpSocket<'_, '_> {
    async fn wait_available(&mut self) -> Result<(), Error> {
        let _ = self.0.wait_recv_available().await;
        Ok(())
    }

    async fn recv_from(&mut self, buffer: &mut [u8]) -> Result<(usize, Address), Error> {
        self.0
            .recv(buffer)
            .await
            .map_err(|_| Error::new(ErrorCode::NoEndpoint))
            .map(|(n, src, _dst)| (n, Address::Udp(SocketAddr::V6(src))))
    }
}
