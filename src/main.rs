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

fn make_socket(interface: &str, local_address: &str) -> UdpSocket {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, None).unwrap();

    if let Err(err) = socket.bind_device(Some(interface.as_bytes())) {
        if matches!(err.raw_os_error(), Some(libc::ENODEV)) {
            panic!("error binding to device (`{}`): {}", interface, err);
        } else {
            panic!("unexpected error binding device: {}", err);
        }
    }


    //let address = socket.local_addr().unwrap();

    let address: SocketAddrV4 = local_address.parse().unwrap();
    socket.bind(&address.into()).unwrap();

    let std_udp: std_udp = socket.into();
    std_udp.set_nonblocking(true).unwrap();

    let udp_socket: UdpSocket = UdpSocket::from_std(std_udp).unwrap();

    udp_socket
}

async fn tun_to_udp(udp_sender: &Arc<UdpSocket>, tun_reader: &mut ReadHalf<tokio_tun::Tun> ) {
    loop {
        let mut buf = [0u8; 1400];
        let n = tun_reader.read(&mut buf).await.unwrap();

        //println!("reading {} bytes: {:?}", n, &buf[..n]);

        let pkt = Packet {
            seq: 0,
            bytes: buf[..n].to_vec()
        };

        let encoded = bincode::serialize(&pkt).unwrap();

        //println!("Encoded length: {}", encoded.len());

        udp_sender.send_to(&encoded, "10.0.0.100:5202").await.unwrap();
    }
}


async fn udp_to_tun(udp_receiver: &Arc<UdpSocket>, tun_sender: &mut WriteHalf<tokio_tun::Tun>) {
    let mut sequence_nr = 0 as usize;

    loop {
        let mut buf = [0; 1500];

        let (len, _addr) = udp_receiver.recv_from(&mut buf).await.unwrap();
        println!("UDP: Received {} bytes", len);


        let decoded: Packet = match bincode::deserialize(&buf[..len]) {
            Ok(result) => {
                result
            },
            Err(_) => {
                // If we receive garbage, simply throw it away and continue.
                continue
            }
        };

        if decoded.seq > sequence_nr {
            sequence_nr = decoded.seq;
            tun_sender.write(&decoded.bytes).await.unwrap();
        }
    }
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

    let (mut reader, mut writer) = tokio::io::split(tun);


    let sock = make_socket("enp5s0", "10.0.0.111:40500");
    let receiver = Arc::new(sock);
    let sender = receiver.clone();

    let mut handles: Vec<task::JoinHandle<_>> = Vec::new();

    handles.push(tokio::spawn(async move {
        tun_to_udp(&sender, &mut reader).await
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
