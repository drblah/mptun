use std::net::Ipv4Addr;
use std::os::unix::io::AsRawFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tun::result::Result;
use tokio_tun::TunBuilder;
use serde::{Serialize, Deserialize};
use bincode;
use socket2::{Domain, SockAddr, Socket, Type};
use tokio::net::UdpSocket;
use std::net::UdpSocket as std_udp;
use std::mem;

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

fn make_socket(interface: &str) -> UdpSocket {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, None).unwrap();

    if let Err(err) = socket.bind_device(Some(interface.as_bytes())) {
        if matches!(err.raw_os_error(), Some(libc::ENODEV)) {
            panic!("error binding to device (`{}`): {}", interface, err);
        } else {
            panic!("unexpected error binding device: {}", err);
        }
    }

    let address = socket.local_addr().unwrap();
    socket.bind(&address.into()).unwrap();

    let std_udp: std_udp = socket.into();
    std_udp.set_nonblocking(true);

    let udp_socket: UdpSocket = UdpSocket::from_std(std_udp).unwrap();

    udp_socket
}


#[tokio::main]
async fn main() {

    let tun = TunBuilder::new()
        .name("")
        .tap(false)
        .packet_info(false)
        .mtu(1350)
        .up()
        .address(Ipv4Addr::new(10, 0, 0, 1))
        .destination(Ipv4Addr::new(10, 1, 0, 1))
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

    println!("---------------------");
    println!("ping 10.1.0.2 to test");
    println!("---------------------");

    let (mut reader, mut _writer) = tokio::io::split(tun);

    let soc = make_socket("enp5s0");

    let _ = tokio::join!(
      tokio::task::spawn(async move {
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

                soc.send_to(&encoded, "10.0.0.100:5202").await;
            }
        })
    );


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
