use std::net::{Ipv4Addr, SocketAddrV4};
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


/*
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Packet<const N: usize> {
    seq: usize,
    part: usize,
    length: usize,
    #[serde(with = "serde_arrays")]
    bytes: [u8; N]
}
*/

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
struct Packet {
    seq: usize,
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


    //let address = socket.local_addr().unwrap();

    let address = SocketAddrV4::new(local_address, local_port);
    socket.bind(&address.into()).unwrap();

    let std_udp: std_udp = socket.into();
    std_udp.set_nonblocking(true).unwrap();

    let udp_socket: UdpSocket = UdpSocket::from_std(std_udp).unwrap();

    udp_socket
}

async fn read_tun(mut tun_reader: ReadHalf<tokio_tun::Tun>, chan_sender: tokio::sync::broadcast::Sender<Packet>) {
    let mut seq: usize = 0;

    loop {
        let mut buf = [0u8; 1400];
        let n = tun_reader.read(&mut buf).await.unwrap();

        let pkt = Packet{
            seq,
            bytes: buf[..n].to_vec()
        };
        seq = seq + 1;

        chan_sender.send(pkt).unwrap();
    }
}

async fn send_tun(mut tun_sender: WriteHalf<tokio_tun::Tun>, mut chan_receiver: tokio::sync::mpsc::UnboundedReceiver::<Packet>) {
    let mut seq: usize = 0;
    loop {
        let packet = chan_receiver.recv().await.unwrap();

        if packet.seq > seq {
            seq = packet.seq;
            tun_sender.write(&packet.bytes).await.unwrap();
        }
    }
}

async fn send_udp(socket: Arc<UdpSocket>, target: SocketAddrV4, mut chan_receiver: tokio::sync::broadcast::Receiver<Packet>) {
    loop {
        let pkt = chan_receiver.recv().await.unwrap();

        let encoded = bincode::serialize(&pkt).unwrap();
        socket.send_to(&encoded, target).await.unwrap();
    }
}

async fn recv_udp(socket: Arc<UdpSocket>, chan_sender: tokio::sync::mpsc::UnboundedSender::<Packet>) {
    loop {
        let mut buf = [0; 1500];
        let (len, _addr) = socket.recv_from(&mut buf).await.unwrap();

        let decoded: Packet = match bincode::deserialize(&buf[..len]) {
            Ok(result) => {
                result
            },
            Err(err) => {
                // If we receive garbage, simply throw it away and continue.
                println!("{}", err);
                continue
            }
        };

        chan_sender.send(decoded).unwrap();
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
    remote_port: u16
}

#[tokio::main]
async fn main() {

    let yaml = load_yaml!("cli.yaml");
    let matches = App::from(yaml).get_matches();

    let conf_path = match matches.value_of("config") {
        Some(value) => value,
        _ => panic!("Failed to get config file path. Does it point to a valid path?")
    };

    let settings: SettingsFile = serde_json::from_str(conf_path).unwrap();

    println!("Using settings: {:?}", settings);

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

    let (tx, _) = tokio::sync::broadcast::channel::<Packet>(10);
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel::<Packet>();


    let mut sockets: Vec<Arc<UdpSocket>> = Vec::new();

    for dev in settings.send_devices {
        let socket = make_socket(dev.udp_iface.as_str(), dev.udp_listen_addr, dev.udp_listen_port);
        sockets.push(Arc::new(socket));
    }

    for socket in sockets {
        let soc_send = socket.clone();
        let soc_recv = soc_send.clone();
        let target = SocketAddrV4::new(settings.remote_addr, settings.remote_port);
        let rx = tx.subscribe();
        task::spawn_blocking(move || {
            send_udp(soc_send, target, rx)
        });

        let tx = inbound_tx.clone();
        task::spawn_blocking(move || {
            recv_udp(soc_recv, tx)
        });
    }

    task::spawn_blocking(|| {
        read_tun(tun_reader, tx)
    });

    task::spawn_blocking(|| {
        send_tun(tun_writer, inbound_rx)
    });



    /*
    let sock = make_socket(settings.udp_iface.as_str(), settings.udp_listen_addr, settings.udp_listen_port);
    let receiver = Arc::new(sock);
    let sender = receiver.clone();

    let mut handles: Vec<task::JoinHandle<_>> = Vec::new();

    handles.push(tokio::spawn(async move {
        tun_to_udp(sender, reader, settings.remote_addr, settings.remote_port).await
    }));

    handles.push(tokio::spawn(async move {
        udp_to_tun(receiver, writer).await
    }));


    for h in handles {
        h.await.unwrap();
    }

     */
    
}
