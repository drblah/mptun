use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Packet {
    pub seq: usize,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>
}