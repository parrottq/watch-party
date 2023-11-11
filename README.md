# Watch Party

Simple client-server watch party software via gstreamer.

## How it works

1. A watch party client connects over tcp to the watch party server
2. Watch party server tells the client which https link to download video from.
3. Each client will play the video file (with gstreamer).
4. Watch party server will tell the clients when to pause/play the video.

## Building
You'll need to install both development and runtime gstreamer ([see here](https://crates.io/crates/gstreamer)).

```bash
cargo build --all
```

## Packaging the Clients
Packaging is a little tricky since gstreamer isn't statically linked. There are two options:
1. Each client installs the gstreamer runtime ([see here](https://gstreamer.freedesktop.org/documentation/installing/index.html?gi-language=c))
2. Find all the needed shared libraries then copy them into the same directory as the executable, distribute them together. ([windows with ListDLL](https://learn.microsoft.com/en-us/sysinternals/downloads/listdlls) and linux using ldd)

## Configuring
### Clients
Put your `ip_address:port` into [`config.json`](wp_client/config.json). Distribute this file in the same directory as the executable.

### Server
1. Forward port tcp/3945
2. Added a video file entry into [`videos.json`](wp_server/videos.json)
