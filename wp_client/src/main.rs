use std::env;

use anyhow::Error;

mod examples_common {
    #[cfg(not(target_os = "macos"))]
    pub fn run<T, F: FnOnce() -> T + Send + 'static>(main: F) -> T
    where
        T: Send + 'static,
    {
        main()
    }

    #[cfg(target_os = "macos")]
    pub fn run<T, F: FnOnce() -> T + Send + 'static>(main: F) -> T
    where
        T: Send + 'static,
    {
        use std::thread;

        use cocoa::appkit::NSApplication;

        unsafe {
            let app = cocoa::appkit::NSApp();
            let t = thread::spawn(|| {
                let res = main();

                let app = cocoa::appkit::NSApp();
                app.stop_(cocoa::base::nil);

                // Stopping the event loop requires an actual event
                let event = cocoa::appkit::NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2_(
                cocoa::base::nil,
                cocoa::appkit::NSEventType::NSApplicationDefined,
                cocoa::foundation::NSPoint { x: 0.0, y: 0.0 },
                cocoa::appkit::NSEventModifierFlags::empty(),
                0.0,
                0,
                cocoa::base::nil,
                cocoa::appkit::NSEventSubtype::NSApplicationActivatedEventType,
                0,
                0,
            );
                app.postEvent_atStart_(event, cocoa::base::YES);

                res
            });

            app.run();

            t.join().unwrap()
        }
    }
}

use gstreamer::ClockTime;
use gstreamer_play::{Play, PlayMessage, PlayVideoRenderer};

fn main_loop(uri: &str) -> Result<(), Error> {
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

fn main() {
    let args: Vec<_> = env::args().collect();
    let uri: &str = if args.len() == 2 {
        args[1].as_ref()
    } else {
        println!("Usage: play uri");
        std::process::exit(-1)
    };

    match main_loop(uri) {
        Ok(r) => r,
        Err(e) => eprintln!("Error! {e}"),
    }
}
