#![feature(async_closure)]

use futures::sink::SinkExt;
use futures::stream::StreamExt;
use nym_chat::EncryptedMessage;
use nym_websocket::responses::ServerResponse;
use std::sync::Arc;
use std::sync::Mutex;
use structopt::StructOpt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;
use warp::Filter;

#[derive(StructOpt)]
struct Options {
    #[structopt(short, long, default_value = "ws://127.0.0.1:1977")]
    websocket: String,
}

fn build_identity_request() -> tokio_tungstenite::tungstenite::Message {
    let nym_message = nym_websocket::requests::ClientRequest::SelfAddress;
    Message::Binary(nym_message.serialize())
}

fn parse_nym_message(
    msg: tokio_tungstenite::tungstenite::Message,
) -> nym_websocket::responses::ServerResponse {
    match msg {
        Message::Binary(bytes) => nym_websocket::responses::ServerResponse::deserialize(&bytes)
            .expect("Could not decode nym client response"),
        msg => panic!("Unexpected message: {:?}", msg),
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let options: Options = Options::from_args();

    debug!("Connecting to websocket at {}", &options.websocket);
    let (mut ws, _) = connect_async(&options.websocket)
        .await
        .expect("Couldn't connect to nym websocket");

    debug!("Requesting own identity from nym client");
    ws.send(build_identity_request())
        .await
        .expect("failed to send identity request");

    // Message logic begins here
    let messages = Arc::new(Mutex::new(Vec::<EncryptedMessage>::new()));

    let server_msgs = messages.clone();
    tokio::spawn(async move {
        let fetch_msg = warp::path!("fetch" / usize).map(move |last_seen| {
            debug!("fetching messages beginning from {}", last_seen);
            // FIXME: DoS bug? out of bound idx
            warp::reply::json::<&[EncryptedMessage]>(&&server_msgs.lock().unwrap()[last_seen..])
        });
        warp::serve(fetch_msg).run(([0, 0, 0, 0], 3030)).await;
    });

    while let Some(Ok(msg)) = ws.next().await {
        let msg = parse_nym_message(msg);

        let msg_bytes = match msg {
            ServerResponse::Received(msg_bytes) => {
                debug!("Received client request {:?}", msg_bytes);
                msg_bytes
            }
            ServerResponse::SelfAddress(addr) => {
                info!("Listening on {}", addr);
                continue;
            }
            ServerResponse::Error(err) => {
                error!("Received error from nym client: {}", err);
                continue;
            }
        };

        match bincode::deserialize(&msg_bytes.message) {
            Ok(msg) => messages.lock().unwrap().push(msg),
            Err(e) => {
                warn!("Could not decode client request");
                debug!("Client request decoding error: {}", e);
                continue;
            }
        };
    }
}
