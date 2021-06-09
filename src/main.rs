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
use clap::{App, load_yaml, ArgMatches};


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

#[derive(Serialize, Deserialize, PartialEq, Debug)]
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

async fn tun_to_udp(udp_sender: &Arc<UdpSocket>, tun_reader: &mut ReadHalf<tokio_tun::Tun>, target_address: Ipv4Addr, target_port: u16 ) {

    let mut seq: usize = 0;
    let target = SocketAddrV4::new(target_address, target_port);
    loop {
        let mut buf = [0u8; 1400];
        let n = tun_reader.read(&mut buf).await.unwrap();

        //println!("reading {} bytes: {:?}", n, &buf[..n]);

        let pkt = Packet {
            seq: seq,
            bytes: buf[..n].to_vec()
        };

        seq = seq + 1;

        let encoded = bincode::serialize(&pkt).unwrap();

        //println!("Encoded length: {}", encoded.len());

        udp_sender.send_to(&encoded, target).await.unwrap();
    }
}


async fn udp_to_tun(udp_receiver: &Arc<UdpSocket>, tun_sender: &mut WriteHalf<tokio_tun::Tun>) {
    let mut sequence_nr = 0 as usize;

    loop {
        let mut buf = [0; 1500];

        let (len, _addr) = udp_receiver.recv_from(&mut buf).await.unwrap();
        //println!("UDP: Received {} bytes", len);

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

        //println!("Parsed: {:?}", decoded);

        if decoded.seq > sequence_nr {
            //println!("Writing the received bytes to tun!");
            sequence_nr = decoded.seq;
            tun_sender.write(&decoded.bytes).await.unwrap();
        }
    }
}

#[derive(Debug)]
struct Settings {
    tun_ip: Ipv4Addr,
    udp_iface: String,
    udp_listen_addr: Ipv4Addr,
    udp_listen_port: u16,
    remote_addr: Ipv4Addr,
    remote_port: u16
}

fn prepare_settings(matches: ArgMatches) -> Settings {

    let tun_ip: Ipv4Addr = match matches.value_of("tun-ip") {
        Some(value) => value.parse().unwrap(),
        _ => panic!("tun-ip is not set.")
    };

    let udp_iface = match matches.value_of("udp-iface") {
        Some(value) => value.to_string(),
        _ => panic!("udp-iface not set")
    };

    let udp_listen_addr: Ipv4Addr = match matches.value_of("udp-listen-addr") {
        Some(value) => value.parse().unwrap(),
        _ => panic!("udp-listen-port not set")
    };

    let udp_listen_port: u16 = match matches.value_of("udp-listen-port") {
        Some(value) => value.parse().unwrap(),
        _ => panic!("udp-listen-port not set")
    };

    let remote_addr: Ipv4Addr = match matches.value_of("remote-addr") {
        Some(value) => value.parse().unwrap(),
        _ => panic!("remote-addr not set")
    };

    let remote_port: u16 = match matches.value_of("remote-port") {
        Some(value) => value.parse().unwrap(),
        _ => panic!("remote-port not set")
    };

    Settings{
        tun_ip,
        udp_iface,
        udp_listen_addr,
        udp_listen_port,
        remote_addr,
        remote_port
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {

    let yaml = load_yaml!("cli.yaml");
    let matches = App::from(yaml).get_matches();

    let settings = prepare_settings(matches);

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

    let (mut reader, mut writer) = tokio::io::split(tun);


    let sock = make_socket(settings.udp_iface.as_str(), settings.udp_listen_addr, settings.udp_listen_port);
    let receiver = Arc::new(sock);
    let sender = receiver.clone();

    let mut handles: Vec<task::JoinHandle<_>> = Vec::new();

    handles.push(tokio::spawn(async move {
        tun_to_udp(&sender, &mut reader, settings.remote_addr, settings.remote_port).await
    }));

    handles.push(tokio::spawn(async move {
        udp_to_tun(&receiver, &mut writer).await
    }));


    for h in handles {
        h.await.unwrap();
    }

    /*
    loop {
        tun_to_udp(&sender, &mut reader).await;
        udp_to_tun(&receiver, &mut writer).await;
    }

     */



    /*
    let _ = tokio::join!(
      tokio::task::spawn(async move {
            loop {
                let mut buf = [0u8; 1400];

                let n = reader.read(&mut buf).await.unwrap();

                println!("reading {} bytes: {:?}", n, &buf[..n]);

                let pkt = Packet{
                    seq: 0,
                    bytes: buf[..n].to_vec()
                };

                let encoded = bincode::serialize(&pkt).unwrap();

                println!("Encoded length: {}", encoded.len());

                sender.send_to(&encoded, "10.0.0.100:5202").await.unwrap();
            }
        })
    );
    */

    /*
    loop {
        let mut buf = [0u8; 1024];

        let n = reader.read(&mut buf).await.unwrap();

        println!("reading {} bytes: {:?}", n, &buf[..n]);

        let pkt = Packet{
            seq: 0,
            bytes: buf.to_vec()
        };

        let encoded = bincode::serialize(&pkt).unwrap();

        println!("Encoded length: {}", encoded.len());

        let _ = soc.send_to(&encoded, "10.0.0.100:5202").await;
    }

     */


}
