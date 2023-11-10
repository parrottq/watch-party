use serde::{Deserialize, Serialize};

pub const MAGIC: &[u8; 4] = b"wpsy";
pub const VERSION: u32 = 0;

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    // Init(u32),
    CurrentTime { id: u32, unix_time_micro: u128 },
}

impl AsRef<Self> for ClientMsg {
    fn as_ref(&self) -> &Self {
        self
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub enum ServerMsg {
    Error(String),
    RequestTime {
        id: u32,
    },
    LoadVideo {
        hash: ContentHash,
        name: String,
        download: Download,
    },
    StartPlayingAt {
        unix_time_micro: u128,
        playback_time_frames: u64,
    },
    PauseAt {
        playback_time_frames: u64,
    },
}

impl AsRef<Self> for ServerMsg {
    fn as_ref(&self) -> &Self {
        self
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub enum Download {
    Https(String),
    None,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub enum ContentHash {
    Sha256(String),
}
