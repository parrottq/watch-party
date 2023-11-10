use std::fs::{create_dir, File};
use std::io::{stdout, Read, Write};
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use futures_util::StreamExt;
use gstreamer::{ClockTime, Registry};
use gstreamer_play::{Play, PlayVideoRenderer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use wp_transaction::{ClientMsg, ContentHash, ServerMsg, MAGIC, VERSION};

async fn send_socket(socket: &mut TcpStream, payload: impl AsRef<ClientMsg>) -> Result<()> {
    let buf: Vec<u8> = postcard::to_allocvec(payload.as_ref())?;

    socket.write_u32_le(buf.len().try_into()?).await?;
    socket.write(&buf).await?;
    socket.flush().await?;

    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct Config {
    host: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config: Config = serde_json::from_reader(File::open("config.json")?)?;

    gstreamer::init()?;
    Registry::get().scan_path(".");

    let play = Play::new(None::<PlayVideoRenderer>);
    let dir = PathBuf::from("video/");
    let _ = create_dir(&dir);
    let dir = dir.canonicalize()?;
    println!("Directory: {:?}", dir);

    let start_time = Instant::now();
    let mut socket = TcpStream::connect(&config.host).await?;
    socket.write(MAGIC).await?;
    socket.write_u32_le(VERSION).await?;
    socket.flush().await?;
    let mut buf = Vec::new();
    loop {
        let size = socket.read_u32_le().await?.try_into()?;
        buf.clear();
        buf.resize(size, 0);
        let mut buf_slice = &mut buf[..size];
        socket.read_exact(&mut buf_slice).await?;
        let msg: ServerMsg = postcard::from_bytes(&buf_slice)?;
        dbg!(&msg);
        match msg {
            ServerMsg::Error(error) => println!("Got error: {}", error),
            ServerMsg::RequestTime { id } => {
                send_socket(
                    &mut socket,
                    ClientMsg::CurrentTime {
                        id,
                        unix_time_micro: start_time.elapsed().as_micros(),
                    },
                )
                .await?;
            }
            ServerMsg::LoadVideo {
                hash,
                name,
                download,
            } => {
                let mut file_path = dir.clone();
                file_path.push(name);
                println!("Loading file: {:?}", file_path);
                if file_path.exists() {
                    let mut file = File::open(&file_path)?;
                    let mut buf = Vec::new();
                    buf.resize(ring::digest::SHA256_OUTPUT_LEN * 1024, 0);
                    let mut hash_context = ring::digest::Context::new(&ring::digest::SHA256);
                    loop {
                        let len = file.read(&mut buf)?;
                        if len == 0 {
                            break;
                        }
                        hash_context.update(&buf[..len]);
                    }

                    let hash_digest = hash_context.finish();
                    let hash_digest = hash_digest.as_ref();
                    let mut hash_digest_str = String::with_capacity(64);
                    hash_digest.into_iter().for_each(|x| {
                        use std::fmt::Write;
                        write!(&mut hash_digest_str, "{x:02x}").unwrap();
                    });
                    let ContentHash::Sha256(hash) = &hash;
                    if &hash_digest_str == hash {
                        let e = file_path.as_os_str().to_os_string();
                        const START: &str = r#"\\?\"#;
                        let e = e.to_string_lossy();
                        assert!(e.starts_with(START), "{e:?}");
                        let e = e.replacen(START, "file:///", 1);
                        play.set_uri(Some(&e));
                        println!("File already exists");
                        play.play();
                        play.pause();
                        play.seek(ClockTime::from_mseconds(0));
                        continue;
                    }
                    println!("Wrong video file download");
                }

                match download {
                    wp_transaction::Download::Https(file_url) => {
                        let response = reqwest::get(file_url).await?;
                        let content_length = response.content_length();
                        println!("Len {:?}", content_length);

                        let mut video_file_stream = response.bytes_stream();

                        let mut file = std::fs::File::create(&file_path)?;
                        let mut hash_context = ring::digest::Context::new(&ring::digest::SHA256);
                        let mut content_progress: u64 = 0;
                        let mut last_pogress = u64::MAX;
                        while let Some(chunk) = video_file_stream.next().await {
                            let chunk = chunk?;
                            content_progress += {
                                let chunk_size = chunk.len();
                                TryInto::<u64>::try_into(chunk_size).unwrap()
                            };
                            if let Some(content_length) = content_length {
                                let progress = content_progress * 100 / content_length;
                                if progress != last_pogress {
                                    last_pogress = progress;
                                    print!(
                                        "  {}%: {}/{}\r",
                                        progress, content_progress, content_length
                                    );
                                    stdout().flush()?;
                                }
                            }
                            file.write(chunk.as_ref()).unwrap();
                            hash_context.update(chunk.as_ref())
                        }
                        let hash_digest = hash_context.finish();
                        let hash_digest = hash_digest.as_ref();
                        let mut hash_digest_str = String::with_capacity(64);
                        hash_digest.into_iter().for_each(|x| {
                            use std::fmt::Write;
                            write!(&mut hash_digest_str, "{x:02x}").unwrap();
                        });
                        let ContentHash::Sha256(hash) = &hash;
                        if &hash_digest_str == hash {
                            let e = file_path.as_os_str().to_os_string();
                            const START: &str = r#"\\?\"#;
                            let e = e.to_string_lossy();
                            assert!(e.starts_with(START), "{e:?}");
                            let e = e.replacen(START, "file:///", 1);
                            play.set_uri(Some(&e));
                            println!("File downloaded");
                            play.play();
                            play.pause();
                            play.seek(ClockTime::from_mseconds(0));
                            continue;
                        }
                    }
                    wp_transaction::Download::None => todo!(),
                }
            }
            ServerMsg::PauseAt {
                playback_time_frames,
            } => {
                play.pause();
                play.seek(ClockTime::from_seconds_f64(
                    playback_time_frames as f64 / 60.0,
                ))
            }
            ServerMsg::StartPlayingAt {
                unix_time_micro,
                playback_time_frames,
            } => {
                play.pause();
                let current_time = start_time.elapsed();
                let target_time = Duration::from_micros(unix_time_micro.try_into()?);
                if let Some(delay) = target_time.checked_sub(current_time) {
                    // Don't know how long that seeking will take so `sleep_until` instead of just delaying
                    let target_instant = start_time + delay;
                    play.seek(ClockTime::from_seconds_f64(
                        playback_time_frames as f64 / 60.0,
                    ));
                    tokio::time::sleep_until(target_instant.into()).await;
                    play.play();
                } else {
                    play.seek(ClockTime::from_seconds_f64(
                        (current_time - target_time).as_secs_f64()
                            + (playback_time_frames as f64 / 60.0),
                    ));
                    play.play();
                }
            }
        }
    }
}
