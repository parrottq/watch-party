use std::time::Instant;
use std::{env, sync::Arc, time::Duration};

use anyhow::Result;

use gstreamer::ClockTime;
use gstreamer_play::{Play, PlayMessage, PlayVideoRenderer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use wp_transaction::{ClientMsg, ServerMsg, MAGIC, VERSION};

async fn send_socket(socket: &mut TcpStream, payload: impl AsRef<ClientMsg>) -> Result<()> {
    let buf: Vec<u8> = postcard::to_allocvec(payload.as_ref())?;

    socket.write_u32_le(buf.len().try_into()?).await?;
    socket.write(&buf).await?;
    socket.flush().await?;

    Ok(())
}

fn main_loop(uri: &str) -> Result<()> {
    gstreamer::init()?;

    let play = Play::new(None::<PlayVideoRenderer>);
    play.set_uri(Some(uri));
    play.play();
    play.seek(ClockTime::from_seconds_f64(1.0));
    dbg!(play.pipeline());

    let mut result = Ok(());
    for msg in play.message_bus().iter_timed(gstreamer::ClockTime::NONE) {
        // dbg!(&msg);
        match PlayMessage::parse(&msg) {
            Ok(PlayMessage::EndOfStream) => {
                play.stop();
                break;
            }
            Ok(PlayMessage::Error { error, details: _ }) => {
                result = Err(error);
                play.stop();
                break;
            }
            Ok(msg) => match &msg {
                PlayMessage::PositionUpdated { position } => {
                    if position.map_or(false, |x| x.seconds_f64() > 3.0) {
                        play.pause();
                    }
                }
                PlayMessage::UriLoaded => (),
                PlayMessage::DurationChanged { duration } => (),
                PlayMessage::StateChanged { state } => (),
                PlayMessage::Buffering { percent } => (),
                PlayMessage::EndOfStream => (),
                PlayMessage::Error { error, details } => todo!(),
                PlayMessage::Warning { error, details } => todo!(),
                PlayMessage::VideoDimensionsChanged { width, height } => todo!(),
                PlayMessage::MediaInfoUpdated { info } => (),
                PlayMessage::VolumeChanged { volume } => (),
                PlayMessage::MuteChanged { muted } => todo!(),
                PlayMessage::SeekDone => todo!(),
                e => {
                    dbg!(e);
                }
            },
            Err(_) => unreachable!(),
        }
    }
    result.map_err(|e| e.into())
}

#[tokio::main]
async fn main() -> Result<()> {
    let start_time = Instant::now();
    let mut socket = TcpStream::connect("127.0.0.1:3945").await?;
    socket.write(MAGIC).await?;
    socket.write_u32_le(VERSION).await?;
    socket.flush().await?;
    let mut buf = Vec::new();
    loop {
        let size = socket.read_u32_le().await?.try_into()?;
        buf.clear();
        // buf.reserve(size);
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
            _ => todo!(),
        }
    }
    todo!();

    let args: Vec<_> = env::args().collect();
    let uri: &str = if args.len() == 2 {
        args[1].as_ref()
    } else {
        println!("Usage: play uri");
        std::process::exit(-1)
    };

    main_loop(uri)?;

    Ok(())
}
