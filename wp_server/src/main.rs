use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{stdin, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, RwLock};
use wp_transaction::{ClientMsg, ContentHash, Download, ServerMsg, MAGIC, VERSION};

async fn send_socket(socket: &mut TcpStream, payload: impl AsRef<ServerMsg>) -> Result<()> {
    let buf: Vec<u8> = postcard::to_allocvec(payload.as_ref())?;

    socket.write_u32_le(buf.len().try_into()?).await?;
    socket.write(&buf).await?;
    socket.flush().await?;

    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct VideoDescription {
    hash: ContentHash,
    name: String,
    download: Download,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    StartPlayingAt {
        unix_time_micro: u128,
        playback_time_frames: u64,
    },
    PauseAt {
        playback_time_frames: u64,
    },
}

impl From<State> for ServerMsg {
    fn from(value: State) -> Self {
        match value {
            State::PauseAt {
                playback_time_frames,
            } => ServerMsg::PauseAt {
                playback_time_frames,
            },
            State::StartPlayingAt {
                unix_time_micro,
                playback_time_frames,
            } => ServerMsg::StartPlayingAt {
                unix_time_micro,
                playback_time_frames,
            },
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:3945").await?;
    let start_time = Arc::new(Instant::now());

    let (broadcast_tx, broadcast_rx) = broadcast::channel(10);
    let current_video = Arc::new(RwLock::new(Option::<(ServerMsg, State)>::None));

    let current_video_clients = current_video.clone();
    let start_time_clients = start_time.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, addr) = listener.accept().await.unwrap();

            let mut retransmit_channel = broadcast_rx.resubscribe();

            let start_time = start_time_clients.clone();
            let current_video = current_video_clients.clone();
            tokio::spawn(async move {
                let mut magic_buf = [0; 4];
                socket.read_exact(&mut magic_buf).await.unwrap();

                if *MAGIC != magic_buf {
                    send_socket(&mut socket, ServerMsg::Error(format!("Bad magic number")))
                        .await
                        .unwrap();
                    panic!("Bad magic number {magic_buf:?}");
                }

                // let mut version_buf = [0; 4];
                let version = socket.read_u32_le().await.unwrap();

                if version != VERSION {
                    send_socket(
                        &mut socket,
                        ServerMsg::Error(format!("Unsupported version")),
                    )
                    .await
                    .unwrap();
                    panic!("Wrong version {version} (supported version {VERSION})");
                }

                println!("{}: Client connected", addr);

                println!("{}: Syncing time...", addr);
                let mut time_id_counter = 0;
                let mut samples = vec![];
                for _ in 0..100 {
                    send_socket(
                        &mut socket,
                        ServerMsg::RequestTime {
                            id: time_id_counter,
                        },
                    )
                    .await
                    .unwrap();
                    let time = Instant::now();

                    let size = socket.read_u32_le().await.unwrap();
                    let delta = time.elapsed();
                    let current_time = start_time.elapsed();

                    let mut buf = vec![0; size.try_into().unwrap()];
                    socket.read_exact(&mut buf).await.unwrap();
                    let res: ClientMsg = postcard::from_bytes(&mut buf).unwrap();
                    let ClientMsg::CurrentTime {
                        id: _,
                        unix_time_micro,
                    } = res;

                    let client_time = Duration::from_micros(unix_time_micro.try_into().unwrap());
                    let estimated_client_delta = current_time - (client_time + delta / 2);
                    // dbg!(delta, current_time, client_time, &res, estimated_client_delta);
                    samples.push((delta, estimated_client_delta));

                    time_id_counter += 1;
                }

                let samples = samples.into_iter().collect::<BTreeMap<_, _>>();
                dbg!(&samples);
                let mut samples_iter = samples.into_iter().rev();
                let (mut _last_error, mean) = samples_iter.next().unwrap();
                let mut mean_time_delta = mean.as_secs_f64();
                for (_error, sample) in samples_iter {
                    // TODO: Measure this based on error somehow to be more statistically rigorous?
                    let sample_importance_ration = 0.25;
                    mean_time_delta = mean_time_delta * (sample_importance_ration)
                        + sample.as_secs_f64() * (1.0 - sample_importance_ration);
                }
                println!("{}: Time delta is {}s", addr, mean_time_delta);

                let mean_time_delta = Duration::from_secs_f64(mean_time_delta);

                {
                    if let Some((loaded_video, state)) = &*current_video.read().await {
                        send_socket(&mut socket, loaded_video).await.unwrap();
                        send_socket(
                            &mut socket,
                            match Into::<ServerMsg>::into(*state) {
                                ServerMsg::StartPlayingAt {
                                    unix_time_micro,
                                    playback_time_frames,
                                } => {
                                    let client_time_micro = (Duration::from_micros(
                                        unix_time_micro.try_into().unwrap(),
                                    ) - mean_time_delta)
                                        .as_micros();
                                    ServerMsg::StartPlayingAt {
                                        unix_time_micro: client_time_micro,
                                        playback_time_frames,
                                    }
                                }
                                other => other,
                            },
                        )
                        .await
                        .unwrap();
                    }
                }

                loop {
                    let command: ServerMsg = retransmit_channel.recv().await.unwrap();
                    let msg = match command {
                        ServerMsg::StartPlayingAt {
                            unix_time_micro,
                            playback_time_frames,
                        } => {
                            let client_time_micro =
                                (Duration::from_micros(unix_time_micro.try_into().unwrap())
                                    - mean_time_delta)
                                    .as_micros();
                            ServerMsg::StartPlayingAt {
                                unix_time_micro: client_time_micro,
                                playback_time_frames,
                            }
                        }
                        msg => msg,
                    };
                    send_socket(&mut socket, msg).await.unwrap();
                }
            });
        }
    });

    loop {
        let mut msg = Vec::new();
        loop {
            let byte = stdin().read_u8().await.unwrap();
            if byte == b'\n' {
                break;
            }
            msg.push(byte);
        }

        let msg = String::from_utf8_lossy(&msg);
        let msg = msg.trim();
        println!("Msg '{}'", msg);

        // TODO: Make milliseconds delay configurable? Or dynamically adapt based on client latencies?
        const PLAYBACK_START_BUF: Duration = Duration::from_millis(500);
        let fps = 60.0;

        match msg {
            "exit" => {
                break;
            }
            "t" => {
                broadcast_tx.send(ServerMsg::Error(format!("wow")))?;
            }
            "" => {
                let mut current_video_guard = current_video.write().await;
                let Some((_loaded_video, state)) = &mut *current_video_guard else {
                    println!("No video loaded.");
                    continue;
                };
                *state = match *state {
                    State::StartPlayingAt {
                        playback_time_frames,
                        unix_time_micro,
                    } => {
                        let time_played_since_pause = start_time
                            .elapsed()
                            .saturating_sub(Duration::from_micros(unix_time_micro.try_into()?)) + PLAYBACK_START_BUF;
                        let time_played_since_pause_frames =
                            (time_played_since_pause.as_secs_f64() * fps) as u64;
                        State::PauseAt {
                            playback_time_frames: playback_time_frames
                                + time_played_since_pause_frames,
                        }
                    }
                    State::PauseAt {
                        playback_time_frames,
                    } => State::StartPlayingAt {
                        unix_time_micro: (start_time.elapsed() + PLAYBACK_START_BUF).as_micros(),
                        playback_time_frames,
                    },
                };
                broadcast_tx.send((*state).into())?;
                println!("State is now {:?}", state);
                match *state {
                    State::PauseAt {
                        playback_time_frames,
                    }
                    | State::StartPlayingAt {
                        playback_time_frames,
                        ..
                    } => println!(
                        "Current time is {:?}",
                        Duration::from_secs_f64(playback_time_frames as f64 / fps)
                    ),
                }
            }
            unknown_command => println!("'{}' is not a command", unknown_command),
        }
    }

    Ok(())
}
