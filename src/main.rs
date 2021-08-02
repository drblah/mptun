use std::net::{Ipv4Addr, SocketAddrV4, SocketAddr, IpAddr};
use std::os::unix::io::AsRawFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio_tun::TunBuilder;
use serde::{Serialize, Deserialize};
use bincode;
use socket2::{Domain, Socket, Type};
use tokio::{net::UdpSocket, task};
use std::net::UdpSocket as std_udp;
use std::{sync::Arc};
use clap::{App, load_yaml};
use serde_json;
use tokio::task::JoinHandle;
use std::collections::HashMap;
use std::sync::RwLock;
use etherparse::{SlicedPacket, InternetSlice};

use std::time::Duration;
use tokio::time;

mod multipathtunnel;
mod settings;

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
struct Packet {
    seq: usize,
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>
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

async fn read_tun(mut tun_reader: ReadHalf<tokio_tun::Tun>, chan_sender: tokio::sync::broadcast::Sender<Packet>) {
    println!("Started [read_tun task]");
    let mut seq: usize = 0;

    loop {
        let mut buf = [0u8; 1400];
        let n = tun_reader.read(&mut buf).await.unwrap();

        let pkt = Packet{
            seq,
            bytes: buf[..n].to_vec()
        };
        seq = seq + 1;

        //println!("Tunnel bytes: {:?}", pkt.bytes);

        chan_sender.send(pkt).unwrap();
    }
}

async fn send_tun(mut tun_sender: WriteHalf<tokio_tun::Tun>, mut chan_receiver: tokio::sync::mpsc::UnboundedReceiver::<Packet>) {
    println!("Started [send_tun task]");
    let mut seq: usize = 0;
    loop {
        let packet = chan_receiver.recv().await.unwrap();

        if packet.seq > seq {
            seq = packet.seq;
            tun_sender.write(&packet.bytes).await.unwrap();
        }
    }
}

async fn send_udp(socket: Arc<UdpSocket>, client_list: Arc<RwLock<HashMap<IpAddr, Vec<SocketAddr>>>>, mut chan_receiver: tokio::sync::broadcast::Receiver<Packet>) {
    println!("Started [send_udp task]");
    loop {
        let pkt: Packet = match chan_receiver.recv().await {
            Ok(pkt) => pkt,
            Err(e) => {
                eprintln!("send_udp task channel overrun. Dropping packets!: {}", e);
                continue
            }
        };

        // Decode IP packet and extract destination TUN IP
        let tun_ip = match SlicedPacket::from_ip(pkt.bytes.as_slice()) {
            Err(value) => {
                eprintln!("Error extracting senders TUN IP: {:?}", value);
                continue;
            },
            Ok(value) => {
                match value.ip {
                    Some(InternetSlice::Ipv4(ipheader)) => {
                        IpAddr::V4(ipheader.destination_addr())
                    },
                    Some(InternetSlice::Ipv6(_, _)) => {
                        eprintln!("TODO: Handle receiving IPv6");
                        continue
                    }
                    None => {continue}

                }
            }
        };

        //println!("Pkt should be sent to: {}", tun_ip);

        let encoded = bincode::serialize(&pkt).unwrap();
        let mut targets: Vec<SocketAddr> = Vec::new();

        {
            let cl = client_list.read().unwrap();

            if let Some(destination) = cl.get(&tun_ip) {
                for target in destination {
                    targets.push(target.clone());
                }
            } else {
                eprintln!("I don't know any destinations for: {}. Perhaps it has not been discovered yet?", tun_ip);
            }
        }

        for target in targets {
            //println!("Sending to: {}", target);
            socket.send_to(&encoded, target).await.unwrap();
        }

    }
}

async fn recv_udp(socket: Arc<UdpSocket>, chan_sender: tokio::sync::mpsc::UnboundedSender::<Packet>, client_list: Arc<RwLock<HashMap<IpAddr, Vec<SocketAddr>>>>) {
    println!("Started [recv_udp task]");
    loop {
        let mut buf = [0; 1500];
        let (len, addr) = socket.recv_from(&mut buf).await.unwrap();

        let decoded: Packet = match bincode::deserialize(&buf[..len]) {
            Ok(result) => {
                result
            },
            Err(err) => {
                // If we receive garbage, simply throw it away and continue.
                println!("Unable do deserialize packet. Got error: {}", err);
                continue
            }
        };

        // Decode IP packet and extract sender's TUN IP
        let tun_ip = match SlicedPacket::from_ip(decoded.bytes.as_slice()) {
            Err(value) => {
                eprintln!("Error extracting senders TUN IP: {:?}", value);
                continue;
            },
            Ok(value) => {
                match value.ip {
                    Some(InternetSlice::Ipv4(ipheader)) => {
                        IpAddr::V4(ipheader.source_addr())
                    },
                    Some(InternetSlice::Ipv6(_, _)) => {
                        eprintln!("TODO: Handle receiving IPv6");
                        continue
                    }
                    None => {continue}

                    }
                }
        };

        let mut cl = client_list.write().unwrap();

        if let Some(client) = cl.get_mut(&tun_ip) {
            if  !client.contains(&addr) {
                client.push(addr);
                println!("Added: IP: {} to existing client: {}.", addr, tun_ip);
            }
        } else {
            cl.insert(tun_ip, vec!(addr) );
            println!("Added new client: {} with IP: {}", tun_ip, addr);
        }

        chan_sender.send(decoded).unwrap();
    }
}

async fn keep_alive(socket: Arc<UdpSocket>, client_list: Arc<RwLock<HashMap<IpAddr, Vec<SocketAddr>>>>, interval: u64) {
    let mut interval = time::interval(Duration::from_secs(interval));

    loop {
        interval.tick().await;

        let mut hosts_to_ping: Vec<SocketAddr> = Vec::new();

        {
            let cl = client_list.read().unwrap();
            for ip in cl.keys() {
                for destinations in cl.get(ip) {
                    for destination in destinations {
                        hosts_to_ping.push(destination.clone());
                    }
                }
            }
        }

        for destination in hosts_to_ping {
            println!("Sending keep-alive packet to: {}", destination);
            socket.send_to(&[0, 0], destination).await.unwrap();
        }
    }
}

#[derive(Deserialize, Debug)]
struct SendDevice {
    udp_iface: String,
    udp_listen_addr: Ipv4Addr,
    udp_listen_port: u16
}

#[derive(Deserialize, Debug)]
struct SettingsFile {
    tun_ip: Ipv4Addr,
    send_devices: Vec<SendDevice>,
    remote_addr: Ipv4Addr,
    remote_port: u16,
    remote_tun_addr: Option<Ipv4Addr>,
    keep_alive: Option<bool>,
    keep_alive_interval: Option<u64>
}

#[tokio::main]
async fn main() {

    let client_list: Arc<RwLock<HashMap<IpAddr, Vec<SocketAddr>>>> = Arc::new(RwLock::new(HashMap::new()));



    let yaml = load_yaml!("cli.yaml");
    let matches = App::from(yaml).get_matches();

    let conf_path = match matches.value_of("config") {
        Some(value) => value,
        _ => panic!("Failed to get config file path. Does it point to a valid path?")
    };

    let settings: SettingsFile = serde_json::from_str(
        std::fs::read_to_string(conf_path)
            .unwrap()
            .as_str()
    ).unwrap();

    println!("Using settings: {:?}", settings);

    // Insert pre-configured clients
    match settings.remote_tun_addr {
        Some(remote) => {
            println!("Inserting pre-configured remote: {}", remote);
            let mut cl = client_list.write().unwrap();
            let socket = SocketAddr::new(IpAddr::V4(settings.remote_addr), settings.remote_port);
            cl.insert(IpAddr::V4(remote), vec![socket]);
        },
        None => {}
    }

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

    let (tun_reader, tun_writer) = tokio::io::split(tun);

    let (tx, _) = tokio::sync::broadcast::channel::<Packet>(200);
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel::<Packet>();


    let mut sockets: Vec<Arc<UdpSocket>> = Vec::new();
    let mut tasks: Vec<JoinHandle<_>> = Vec::new();

    for dev in settings.send_devices {
        let socket = make_socket(dev.udp_iface.as_str(), dev.udp_listen_addr, dev.udp_listen_port);
        sockets.push(Arc::new(socket));
    }

    for socket in sockets {
        let soc_send = socket.clone();
        let soc_recv = soc_send.clone();

        let rx = tx.subscribe();

        let send_client_list = client_list.clone();
        let recv_client_list = send_client_list.clone();


        match settings.keep_alive {
            Some(should_keep_alive) => {
                if should_keep_alive {
                    let keep_alive_soc = soc_recv.clone();
                    let keep_alive_client_list = recv_client_list.clone();
                    let interval = settings.keep_alive_interval.unwrap();

                    tasks.push(task::spawn(async move {
                        keep_alive(keep_alive_soc, keep_alive_client_list, interval).await
                    }));
                }
            },
            None => {}
        }

        tasks.push(task::spawn(async move {
            send_udp(soc_send, send_client_list, rx).await
        }));

        let tx = inbound_tx.clone();
        tasks.push(task::spawn(async move {
            recv_udp(soc_recv, tx, recv_client_list).await
        }));
    }

    tasks.push(task::spawn(async move {
        read_tun(tun_reader, tx).await
    }));

    tasks.push(task::spawn(async move {
        send_tun(tun_writer, inbound_rx).await
    }));

    for task in tasks {
        task.await.unwrap();
    }
    
}
