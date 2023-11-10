use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, EventStream, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use futures_util::StreamExt;
use log::{debug, error, info};
use ratatui::prelude::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Widget};
use ratatui::{
    prelude::{CrosstermBackend, Terminal},
    widgets::Paragraph,
};
use serde::{Deserialize, Serialize};
use std::io::stdout;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{broadcast, RwLock};
use wp_transaction::{ClientMsg, ContentHash, Download, ServerMsg, ServingVideo, MAGIC, VERSION};

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

static LOGGER: LogQueue = LogQueue(OnceLock::new());

struct LogQueue(OnceLock<UnboundedSender<String>>);

impl log::Log for LogQueue {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        self.0.get().is_some()
    }

    fn log(&self, record: &log::Record) {
        if let Some(log_queue) = self.0.get() {
            log_queue.send(format!("{}", record.args())).unwrap();
        }
    }

    fn flush(&self) {
        // Doesn't need to be flushed
    }
}

struct LogBuffer {
    queue: VecDeque<String>,
    max_len: usize,
}

impl LogBuffer {
    fn new() -> Self {
        Self {
            queue: Default::default(),
            max_len: 100,
        }
    }

    fn append_entry(&mut self, msg: String) {
        for msg_part in msg.split('\n') {
            for _ in 0..(self.queue.len() + 1).saturating_sub(self.max_len) {
                self.queue.pop_front();
            }
            self.queue.push_back(msg_part.to_string());
        }
    }

    async fn recv_entries(&mut self, receiver: &mut UnboundedReceiver<String>) {
        self.append_entry(receiver.recv().await.unwrap());

        // Process more message opportunistically for better perf
        self.try_recv_entries(receiver);
    }

    fn try_recv_entries(&mut self, receiver: &mut UnboundedReceiver<String>) {
        while let Ok(msg) = receiver.try_recv() {
            self.append_entry(msg);
        }
    }

    fn iter(&self) -> impl Iterator<Item = &str> {
        self.queue.iter().rev().map(AsRef::as_ref)
    }
}

struct LogBufferWidget<'a>(&'a LogBuffer);

impl<'a> Widget for LogBufferWidget<'a> {
    fn render(self, area: ratatui::prelude::Rect, buf: &mut ratatui::prelude::Buffer) {
        for (i, text) in (0..area.height).rev().zip(self.0.iter()) {
            // TODO: Fix style
            buf.set_stringn(
                area.x,
                area.y + i,
                text,
                area.width.into(),
                Style::default(),
            );
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let (log_sender, mut log_receiver) = mpsc::unbounded_channel();
    LOGGER.0.set(log_sender).unwrap();
    log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Debug))?;

    let listener = TcpListener::bind("0.0.0.0:3945").await?;
    let start_time = Arc::new(Instant::now());

    let (broadcast_tx, broadcast_rx) = broadcast::channel(10);
    let current_video = Arc::new(RwLock::new(Option::<(ServingVideo, State)>::None));

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

                info!("{}: Client connected", addr);

                info!("{}: Syncing time...", addr);
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
                debug!("{:#?}", &samples);
                let mut samples_iter = samples.into_iter().rev();
                let (mut _last_error, mean) = samples_iter.next().unwrap();
                let mut mean_time_delta = mean.as_secs_f64();
                for (_error, sample) in samples_iter {
                    // TODO: Measure this based on error somehow to be more statistically rigorous?
                    let sample_importance_ration = 0.25;
                    mean_time_delta = mean_time_delta * (sample_importance_ration)
                        + sample.as_secs_f64() * (1.0 - sample_importance_ration);
                }
                info!("{}: Time delta is {}s", addr, mean_time_delta);

                let mean_time_delta = Duration::from_secs_f64(mean_time_delta);

                {
                    if let Some((loaded_video, state)) = &*current_video.read().await {
                        send_socket(&mut socket, Into::<ServerMsg>::into(loaded_video.clone()))
                            .await
                            .unwrap();
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

    // TODO: Make milliseconds delay configurable? Or dynamically adapt based on client latencies?
    const PLAYBACK_START_BUF: Duration = Duration::from_millis(500);
    let fps = 60.0;

    stdout().execute(EnterAlternateScreen)?;
    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    info!("Intialized.");

    let mut log_buffer = LogBuffer::new();
    let mut event_stream = EventStream::new();
    loop {
        let event = tokio::select! {
            event = event_stream.next() => {
                Some(event.unwrap()?)
            },
            _ = log_buffer.recv_entries(&mut log_receiver) => {
                None
            }
        };

        if let Some(event::Event::Key(key)) = event {
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // Exit
                        break;
                    }
                    KeyCode::Char(' ') => {
                        // Toggle pause/play
                        let mut current_video_guard = current_video.write().await;
                        if let Some((_loaded_video, state)) = &mut *current_video_guard {
                            *state = match *state {
                                State::StartPlayingAt {
                                    playback_time_frames,
                                    unix_time_micro,
                                } => {
                                    let time_played_since_pause =
                                        start_time.elapsed().saturating_sub(Duration::from_micros(
                                            unix_time_micro.try_into()?,
                                        )) + PLAYBACK_START_BUF;
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
                                    unix_time_micro: (start_time.elapsed() + PLAYBACK_START_BUF)
                                        .as_micros(),
                                    playback_time_frames,
                                },
                            };
                            broadcast_tx.send((*state).into())?;
                            info!("State is now {:?}", state);
                            match *state {
                                State::PauseAt {
                                    playback_time_frames,
                                }
                                | State::StartPlayingAt {
                                    playback_time_frames,
                                    ..
                                } => info!(
                                    "Current time is {:?}",
                                    Duration::from_secs_f64(playback_time_frames as f64 / fps)
                                ),
                            }
                        } else {
                            info!("No video loaded.");
                        };
                    }
                    KeyCode::Char(num @ '1'..='9') => {
                        let res = async {
                            let mut file = File::open("videos.json").map_err(|err| {
                                anyhow::anyhow!("Failed to load `videos.json`: {}", err)
                            })?;
                            let videos: HashMap<u8, ServingVideo> =
                                serde_json::from_reader(&mut file)?;
                            let num = num.to_digit(10).unwrap().try_into().unwrap();
                            if let Some(msg) = videos.get(&num) {
                                {
                                    let mut current_video_guard = current_video.write().await;
                                    *current_video_guard = Some((
                                        msg.clone(),
                                        State::PauseAt {
                                            playback_time_frames: 0,
                                        },
                                    ));
                                }
                                broadcast_tx.send(msg.clone().into())?;
                                info!("Video loaded: '{}'", msg.name);
                                Ok(())
                            } else {
                                Err(anyhow::anyhow!("Not video entry '{}'", num))
                            }
                        };

                        match res.await {
                            Ok(_) => {}
                            Err(err) => error!("{}", err),
                        }
                    }
                    KeyCode::Char('t') => {
                        broadcast_tx.send(ServerMsg::Error(format!("wow")))?;
                    }
                    _ => continue, // Don't repaint since event was ignored
                }
            } else {
                continue; // Don't repaint since event was ignored
            }
        }

        // Some log messages might have been sent while handling events. Process them before repainting.
        log_buffer.try_recv_entries(&mut log_receiver);

        let title = {
            let current_video_guard = current_video.read().await;
            if let Some((video, state)) = &*current_video_guard {
                match state {
                    State::StartPlayingAt { .. } => format!("Playing: {seek} '{}'", video.name),
                    State::PauseAt { .. } => format!("Paused:  {seek} '{}'", video.name),
                }
            } else {
                format!("Player: No video loaded")
            }
        };

        terminal.draw(|frame| {
            let main_layout = Layout::default()
                .direction(ratatui::prelude::Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .split(frame.size());

            // Show playing state here
            frame.render_widget(Paragraph::new(title), main_layout[0]);

            let border = Block::default().title("log").borders(Borders::ALL);
            frame.render_widget(LogBufferWidget(&log_buffer), border.inner(main_layout[1]));
            frame.render_widget(border, main_layout[1]);

            frame.render_widget(
                Paragraph::new("q/esc (quit) | 1-9 (video selection) | space (toggle playback)"),
                main_layout[2],
            );
        })?;
    }

    stdout().execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;

    Ok(())
}
