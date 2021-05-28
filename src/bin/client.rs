use futures::SinkExt;
use nym_addressing::clients::Recipient;
use nym_chat::{EncryptedMessage, Key, Message};
use structopt::StructOpt;
use tokio::select;
use tokio::time::Duration;
use tokio_tungstenite::connect_async;

#[derive(StructOpt)]
struct Options {
    #[structopt(short, long, default_value = "ws://127.0.0.1:1977")]
    websocket: String,
    #[structopt(
    short,
    long,
    parse(try_from_str = Recipient::try_from_base58_string),
    )]
    service_provider: Recipient,
    url: String,
    room: Key,
    name: String,
}

#[tokio::main]
async fn main() {
    let opts: Options = StructOpt::from_args();
    let Options {
        websocket,
        service_provider,
        url,
        room,
        name,
    } = opts;

    let (mut ws, _) = connect_async(&websocket)
        .await
        .expect("Couldn't connect to nym websocket");

    let (incoming_send, incoming_receive) = tokio::sync::mpsc::channel::<Message>(16);
    let (outgoing_send, mut outgoing_receive) = tokio::sync::mpsc::channel::<String>(16);

    tokio::spawn(async move {
        let mut idx = 0;
        loop {
            idx += 1;
            tokio::time::sleep(Duration::from_secs(4)).await;
            outgoing_send.send(format!("test {}", idx)).await.unwrap();
        }
    });

    tokio::spawn(async move {
        let mut incoming_receive = incoming_receive;
        while let Some(msg) = incoming_receive.recv().await {
            println!("{}: {}", msg.sender, msg.msg);
        }
    });

    let mut fetch_timer = tokio::time::interval(Duration::from_secs(1));
    let mut last_fetch = 0;

    loop {
        select! {
            Some(msg) = outgoing_receive.recv() => {
                let msg = Message::new(name.clone(), msg);
                let enc_msg = msg.encrypt(&room);
                let nym_packet = nym_websocket::requests::ClientRequest::Send {
                    recipient: service_provider,
                    message: bincode::serialize(&enc_msg).expect("can't fail"),
                    with_reply_surb: false,
                };
                ws.send(tokio_tungstenite::tungstenite::Message::Binary(nym_packet.serialize()))
                    .await
                    .expect("couldn't send request");
            },
            _ = fetch_timer.tick() => {
                let msgs = fetch_messages(&url, last_fetch).await;
                last_fetch += msgs.len();
                for msg in msgs {
                    if let Ok(msg) = Message::decrypt(msg, &room) {
                        incoming_send.send(msg).await.unwrap();
                    }
                }
            }
        }
    }

    ws.close(None).await.expect("Failed to close websocket.");
}

async fn fetch_messages(base_url: &str, last_seen: usize) -> Vec<EncryptedMessage> {
    let client = reqwest::Client::new();
    client
        .get(format!("{}/fetch/{}", base_url, last_seen))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
