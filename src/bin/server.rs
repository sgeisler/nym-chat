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
    /// The nym native client to use
    #[structopt(short, long, default_value = "ws://127.0.0.1:1977")]
    websocket: String,
}

#[tokio::main]
async fn main() {
    // Start the logging framework
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Parse command line options
    let options: Options = Options::from_args();

    // Open a connection to the nym native client and query our own identity
    debug!("Connecting to websocket at {}", &options.websocket);
    let (mut ws, _) = connect_async(&options.websocket)
        .await
        .expect("Couldn't connect to nym websocket");

    debug!("Requesting own identity from nym client");
    ws.send(build_identity_request())
        .await
        .expect("failed to send identity request");

    // Message logic begins here

    // First we create the message database that will contain all messages ever sent. For now this
    // is just a vector inside a mutex to manage access. In a real application it should be a
    // persistent database.
    let messages = Arc::new(Mutex::new(Vec::<EncryptedMessage>::new()));

    // Spawn a webserver that clients will use to sync up messages sent since they last checked.
    // This happens without any privacy measures since everyone is querying all messages, so nothing
    // can be learnt other than someone is using the chat service. No metadata about communication
    // relations is leaked.
    //
    // Note that the recipient privacy is thus weaker than the sender privacy provided by Nym
    // because the anonymity set is merely everyone using this particular service and not every
    // other Nym user. Ideally this could be replaced with a SURB-based protocol once the we know
    // how to build these safely.
    let server_msgs = messages.clone();
    tokio::spawn(async move {
        let fetch_msg = warp::path!("fetch" / usize).map(move |last_seen| {
            debug!("fetching messages beginning from {}", last_seen);
            // FIXME: DoS bug? out of bound idx
            warp::reply::json::<&[EncryptedMessage]>(&&server_msgs.lock().unwrap()[last_seen..])
        });
        warp::serve(fetch_msg).run(([0, 0, 0, 0], 3030)).await;
    });

    // We also listen for incoming Nym messages in parallel. If we receive one that is a valid
    // encrypted message we save it in the message database for clients to query. There is a lot
    // of error management going on that should probably be refactored out.
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
