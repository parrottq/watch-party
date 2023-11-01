use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};

#[tokio::main]
async fn main() -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:3945").await?;

    let (broadcast_tx, broadcast_rx): (broadcast::Sender<Bytes>, _) = broadcast::channel(10);
    let (retransmit_queue_tx, mut retransmit_queue_rx) = mpsc::channel(20);
    let retransmit_queue_tx = Arc::new(retransmit_queue_tx);

    tokio::spawn(async move {
        // Forward packets from any connection to all other sockets
        loop {
            let data = retransmit_queue_rx.recv().await.unwrap();
            broadcast_tx.send(data).unwrap();
        }
    });

    loop {
        let (socket, _addr) = listener.accept().await?;
        let (mut socket_recv, mut socket_send) = tokio::io::split(socket);

        let mut retransmit_channel = broadcast_rx.resubscribe();

        tokio::spawn(async move {
            loop {
                // Take broadcasted packets and send back to client
                let res = retransmit_channel.recv().await;
                socket_send.write(res.unwrap().as_ref()).await.unwrap();
                socket_send.flush().await.unwrap();
            }
        });

        let retransmit_queue_tx = retransmit_queue_tx.clone();
        tokio::spawn(async move {
            // Receive packets from client and send to the rebroadcaster task
            let mut buf = [0; 1024];

            loop {
                let mut size: usize = socket_recv.read_u16().await.unwrap().into();
                if size > buf.len() {
                    // Ignore if the packet is too big
                    while size > 0 {
                        let n = size.min(buf.len());
                        socket_recv.read_exact(&mut buf[..n]).await.unwrap();
                        size = size.saturating_sub(n);
                    }
                } else {
                    let buf = &mut buf[..size];
                    socket_recv.read_exact(buf).await.unwrap();

                    let mut owned_buf = Vec::with_capacity(2 + buf.len());
                    owned_buf.extend(u16::to_be_bytes(size as u16));
                    owned_buf.extend_from_slice(buf);

                    retransmit_queue_tx.send(Bytes::from(owned_buf)).await.unwrap();
                }
            }
        });
    }

    Ok(())
}
