use tokio_tun::TunBuilder;
use std::net::{Ipv4Addr,
               SocketAddrV4,
               SocketAddr,
               IpAddr};
use std::sync::{Arc, RwLock};
use std::os::unix::io::AsRawFd;
use std::collections::HashMap;
use tokio::{net::UdpSocket,
            task};
use socket2::{Domain, Socket, Type};
use std::net::UdpSocket as std_udp;

use crate::settings::SettingsFile;
use crate::tasks;
use crate::packet::Packet;

pub struct Multipathtunnel {
    sockets: Vec<Arc<UdpSocket>>,
    client_list: Arc<RwLock<HashMap<IpAddr, Vec<SocketAddr>>>>
}



pub async fn run(settings: SettingsFile) {

    let settings = Arc::new(settings);

    let mptun = Multipathtunnel{
        sockets: make_sockets(settings.clone()).await,
        client_list: Arc::new(RwLock::new(HashMap::new()))
    };

    // Insert pre-configured clients
    match settings.remote_tun_addr {
        Some(remote) => {
            println!("Inserting pre-configured remote: {}", remote);
            let mut cl = mptun.client_list.write().unwrap();
            let socket = SocketAddr::new(IpAddr::V4(settings.remote_addr), settings.remote_port);
            cl.insert(IpAddr::V4(remote), vec![socket]);
        },
        None => {}
    }


    let mut tasks = Vec::new();

    let tun = make_tunnel(settings.clone()).await;

    let (tun_reader, tun_writer) = tokio::io::split(tun);

    let (tx, _) = tokio::sync::broadcast::channel::<Packet>(200);
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel::<Packet>();

    for socket in mptun.sockets {
        let soc_send = socket.clone();
        let soc_recv = soc_send.clone();

        let rx = tx.subscribe();

        let send_client_list = mptun.client_list.clone();
        let recv_client_list = send_client_list.clone();


        match settings.keep_alive {
            Some(should_keep_alive) => {
                if should_keep_alive {
                    let keep_alive_soc = soc_recv.clone();
                    let keep_alive_client_list = recv_client_list.clone();
                    let interval = settings.keep_alive_interval.unwrap();

                    tasks.push(task::spawn(async move {
                        tasks::keep_alive(keep_alive_soc, keep_alive_client_list, interval).await
                    }));
                }
            },
            None => {}
        }

        tasks.push(task::spawn(async move {
            tasks::send_udp(soc_send, send_client_list, rx).await
        }));

        let tx = inbound_tx.clone();
        tasks.push(task::spawn(async move {
            tasks::recv_udp(soc_recv, tx, recv_client_list).await
        }));
    }

    tasks.push(task::spawn(async move {
        tasks::read_tun(tun_reader, tx).await
    }));

    tasks.push(task::spawn(async move {
        tasks::send_tun(tun_writer, inbound_rx).await
    }));

    for task in &mut tasks {
        task.await.unwrap();
    }
}

async fn make_tunnel(settings: Arc<SettingsFile>) -> tokio_tun::Tun {
    let tun = TunBuilder::new()
        .name("")
        .tap(false)
        .packet_info(false)
        .mtu(1350)
        .up()
        .address(settings.tun_ip)
        .broadcast(Ipv4Addr::BROADCAST)
        .netmask(Ipv4Addr::new(255, 255, 255, 0))
        .try_build().unwrap();

    println!("-----------");
    println!("tun created");
    println!("-----------");

    println!(
        "┌ name: {}\n├ fd: {}\n├ mtu: {}\n├ flags: {}\n├ address: {}\n├ destination: {}\n├ broadcast: {}\n└ netmask: {}",
        tun.name(),
        tun.as_raw_fd(),
        tun.mtu().unwrap(),
        tun.flags().unwrap(),
        tun.address().unwrap(),
        tun.destination().unwrap(),
        tun.broadcast().unwrap(),
        tun.netmask().unwrap(),
    );

    tun
}

fn make_socket(interface: &str, local_address: Ipv4Addr, local_port: u16) -> UdpSocket {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, None).unwrap();

    if let Err(err) = socket.bind_device(Some(interface.as_bytes())) {
        if matches!(err.raw_os_error(), Some(libc::ENODEV)) {
            panic!("error binding to device (`{}`): {}", interface, err);
        } else {
            panic!("unexpected error binding device: {}", err);
        }
    }

    let address = SocketAddrV4::new(local_address, local_port);
    socket.bind(&address.into()).unwrap();

    let std_udp: std_udp = socket.into();
    std_udp.set_nonblocking(true).unwrap();

    let udp_socket: UdpSocket = UdpSocket::from_std(std_udp).unwrap();

    udp_socket
}

async fn make_sockets(settings: Arc<SettingsFile>) -> Vec<Arc<UdpSocket>> {
    let mut sockets: Vec<Arc<UdpSocket>> = Vec::new();

    for dev in &settings.send_devices {
        let socket = make_socket(dev.udp_iface.as_str(), dev.udp_listen_addr, dev.udp_listen_port);
        sockets.push(Arc::new(socket));
    }

    sockets
}

