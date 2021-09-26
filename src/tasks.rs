use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use std::net::{SocketAddr,
               IpAddr};
use etherparse::{SlicedPacket, InternetSlice};
use std::time::Duration;
use tokio::time;
use tokio::{net::UdpSocket};
use lz4_flex::{compress_prepend_size, decompress_size_prepended};

use crate::messages::{Packet, Keepalive, Messages};

pub async fn read_tun(mut tun_reader: ReadHalf<tokio_tun::Tun>, chan_sender: tokio::sync::broadcast::Sender<Packet>) {
    println!("Started [read_tun task]");
    let mut seq: usize = 0;

    loop {
        let mut buf = [0u8; 1500];
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

pub async fn send_tun(mut tun_sender: WriteHalf<tokio_tun::Tun>, mut chan_receiver: tokio::sync::mpsc::UnboundedReceiver::<Packet>) {
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

pub async fn send_udp(socket: Arc<UdpSocket>, client_list: Arc<RwLock<HashMap<IpAddr, Vec<SocketAddr>>>>, mut chan_receiver: tokio::sync::broadcast::Receiver<Packet>) {
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
        let compressed_pkt = Packet{
            seq: pkt.seq,
            bytes: compress_prepend_size(&pkt.bytes)
        };

        let encoded = bincode::serialize(&Messages::Packet(compressed_pkt)).unwrap();
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

pub async fn recv_udp(socket: Arc<UdpSocket>, chan_sender: tokio::sync::mpsc::UnboundedSender::<Packet>, client_list: Arc<RwLock<HashMap<IpAddr, Vec<SocketAddr>>>>) {
    println!("Started [recv_udp task]");
    loop {
        let mut buf = [0; 1500];
        let (len, addr) = socket.recv_from(&mut buf).await.unwrap();

        let decoded: Packet = match bincode::deserialize::<Messages>(&buf[..len]) {
            Ok(decoded) => {
                match decoded {
                    Messages::Packet(pkt) => {
                        Packet{
                            seq: pkt.seq,
                            bytes: decompress_size_prepended(&pkt.bytes).unwrap()
                        }
                    },
                    Messages::Keepalive => {
                        println!("Received keepalive msg.");
                        continue
                    }
                }
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

pub async fn keep_alive(socket: Arc<UdpSocket>, client_list: Arc<RwLock<HashMap<IpAddr, Vec<SocketAddr>>>>, interval: u64) {
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

            let keepalive_msg = bincode::serialize(&Messages::Keepalive).unwrap();
            socket.send_to(keepalive_msg.as_slice(), destination).await.unwrap();
        }
    }
}