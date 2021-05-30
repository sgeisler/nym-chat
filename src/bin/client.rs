use futures::SinkExt;
use nym_addressing::clients::Recipient;
use nym_chat::{EncryptedMessage, Key, Message};
use std::time::Instant;
use structopt::StructOpt;
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::Duration;
use tokio_tungstenite::connect_async;
use tuirealm::tui::widgets::canvas::Context;

// Command line options
#[derive(StructOpt)]
struct Options {
    // Nym client to use
    #[structopt(short, long, default_value = "ws://127.0.0.1:1977")]
    websocket: String,
    // The server's Nym address
    #[structopt(
    short,
    long,
    parse(try_from_str = Recipient::try_from_base58_string),
    )]
    service_provider: Recipient,
    // The server's HTTP server to query the messages from
    url: String,
    // The key defining the chatroom (32 bytes hex encoded)
    room: Key,
    // Our name to be attached to messages
    name: String,
}

#[tokio::main]
async fn main() {
    // Parse command line arguments
    let opts: Options = StructOpt::from_args();
    let Options {
        websocket,
        service_provider,
        url,
        room,
        name,
    } = opts;

    // Connect to Nym native client
    let (mut ws, _) = connect_async(&websocket)
        .await
        .expect("Couldn't connect to nym websocket");

    // Channels to communicate with the UI: the UI can send outgoing message to our main thread
    // and we will encapsulate and encrypt them correctly and it can receive messages that the main
    // thread received and could decrypt. This makes the UI mostly decoupled from the rest of the
    // application.
    let (incoming_send, incoming_receive) = tokio::sync::mpsc::channel::<Message>(16);
    let (outgoing_send, mut outgoing_receive) = tokio::sync::mpsc::channel::<String>(16);

    // Spawn the UI thread, I view this as a blackbox since UI stuff is weird and it is mostly
    // just copy+pasted code.
    let mut ui = tokio::task::spawn_blocking(|| ui::run_ui(incoming_receive, outgoing_send));

    // Start a timer that will wake up the main thread once a second to fetch messages from the server
    let mut fetch_timer = tokio::time::interval(Duration::from_secs(1));
    // Last message fetched from the server, so we only fetch the new ones next time
    let mut last_fetch = 0;

    // Run forever and wait for one of the following events to happen:
    loop {
        select! {
            // The UI thread sent a message, we have to encrypt it and send it via the Nym client
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
            // The fetch timer woke us up, we have to fetch new messages from the server and send
            // the ones we could decrypt to the UI thread.
            _ = fetch_timer.tick() => {
                let msgs = fetch_messages(&url, last_fetch).await;
                last_fetch += msgs.len();
                for msg in msgs {
                    if let Ok(msg) = Message::decrypt(msg, &room) {
                        incoming_send.send(msg).await.unwrap();
                    }
                }
            },
            // The UI thread exited, we exit the infinite loop to stop the application
            _ = &mut ui => {
                break;
            }
        }
    }

    // Gracefully disconnect from the Nym native client
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

// Black magic
pub mod ui {
    use nym_chat::Message;
    use tokio::sync::mpsc::{Receiver, Sender};

    use crossterm::event::DisableMouseCapture;
    use crossterm::event::{poll, read, Event};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };

    use std::io::{stdout, Stdout};
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use tuirealm::components::{input, Table, TablePropsBuilder};
    use tuirealm::props::borders::{BorderType, Borders};
    use tuirealm::{InputType, Msg, Payload, PropPayload, PropValue, PropsBuilder, Value, View};

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tuirealm::props::TextSpan;
    use tuirealm::tui::backend::CrosstermBackend;
    use tuirealm::tui::layout::{Constraint, Direction, Layout};
    use tuirealm::tui::style::Color;
    use tuirealm::tui::Terminal;

    pub const MSG_KEY_ESC: Msg = Msg::OnKey(KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
    });

    const CHAT_LOG: &str = "CHAT_LOG";
    const INPUT_BOX: &str = "INPUT_BOX";

    pub(crate) struct InputHandler;

    impl InputHandler {
        pub fn new() -> InputHandler {
            InputHandler {}
        }

        pub fn read_event(&self) -> Result<Option<Event>, ()> {
            if let Ok(available) = poll(Duration::from_millis(10)) {
                match available {
                    true => {
                        // Read event
                        if let Ok(ev) = read() {
                            Ok(Some(ev))
                        } else {
                            Err(())
                        }
                    }
                    false => Ok(None),
                }
            } else {
                Err(())
            }
        }
    }

    pub struct Context {
        pub(crate) input_hnd: InputHandler,
        pub(crate) terminal: Terminal<CrosstermBackend<Stdout>>,
    }

    impl Context {
        pub fn new() -> Context {
            let _ = enable_raw_mode();
            // Create terminal
            let mut stdout = stdout();
            assert!(execute!(stdout, EnterAlternateScreen).is_ok());
            Context {
                input_hnd: InputHandler::new(),
                terminal: Terminal::new(CrosstermBackend::new(stdout)).unwrap(),
            }
        }

        pub fn enter_alternate_screen(&mut self) {
            let _ = execute!(
                self.terminal.backend_mut(),
                EnterAlternateScreen,
                DisableMouseCapture
            );
        }

        pub fn leave_alternate_screen(&mut self) {
            let _ = execute!(
                self.terminal.backend_mut(),
                LeaveAlternateScreen,
                DisableMouseCapture
            );
        }

        pub fn clear_screen(&mut self) {
            let _ = self.terminal.clear();
        }
    }

    impl Drop for Context {
        fn drop(&mut self) {
            // Re-enable terminal stuff
            self.leave_alternate_screen();
            let _ = disable_raw_mode();
        }
    }

    // Let's create the model

    struct Model {
        quit: bool,
        redraw: Arc<AtomicBool>,
        messages: Arc<Mutex<Vec<Message>>>,
        send: Sender<String>,
    }

    // -- view

    fn view(ctx: &mut Context, view: &View) {
        let _ = ctx.terminal.draw(|f| {
            // Prepare chunks
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Length(3), Constraint::Length(5)].as_ref())
                .split(f.size());

            view.render(INPUT_BOX, f, chunks[0]);
            view.render(CHAT_LOG, f, chunks[1]);
        });
    }

    // -- update

    fn update(
        model: &mut Model,
        view: &mut View,
        msg: Option<(String, Msg)>,
    ) -> Option<(String, Msg)> {
        let ref_msg: Option<(&str, &Msg)> = msg.as_ref().map(|(s, msg)| (s.as_str(), msg));
        match ref_msg {
            None => None, // Exit after None
            Some(msg) => match msg {
                (INPUT_BOX, Msg::OnSubmit(Payload::One(Value::Str(input)))) => {
                    model.send.blocking_send(input.clone()).unwrap();
                    let mut input_props = view.get_props(INPUT_BOX).unwrap();
                    input_props.value = PropPayload::One(PropValue::Str(String::new()));
                    view.update(INPUT_BOX, input_props);
                    None
                }
                (_, &MSG_KEY_ESC) => {
                    // Quit on esc
                    model.quit = true;
                    None
                }
                _ => None,
            },
        }
    }

    pub fn run_ui(mut incoming: Receiver<Message>, outgoing: Sender<String>) {
        let mut ctx: Context = Context::new();
        // We need to setup the terminal, entering alternate screen
        ctx.enter_alternate_screen();
        ctx.clear_screen();
        // Let's create a View
        let mut myview: View = View::init();
        // Let's mount all the components we need
        myview.mount(
            CHAT_LOG,
            Box::new(Table::new(
                TablePropsBuilder::default()
                    .with_table(
                        Some("Messages".into()),
                        vec![vec![TextSpan::from("Nothing here yet …")]],
                    )
                    .build(),
            )),
        );
        myview.mount(
            INPUT_BOX,
            Box::new(input::Input::new(
                input::InputPropsBuilder::default()
                    .with_input(InputType::Text)
                    .with_label(String::from("Send Message"))
                    .build(),
            )),
        );
        // ...
        // Give focus to our component
        myview.active(INPUT_BOX);
        // Prepare states

        let messages = Arc::new(Mutex::new(vec![]));
        let redraw = Arc::new(AtomicBool::new(false));

        let mut states: Model = Model {
            quit: false,
            redraw: redraw.clone(),
            messages: messages.clone(),
            send: outgoing,
        };

        tokio::spawn(async move {
            while let Some(msg) = incoming.recv().await {
                messages.lock().unwrap().push(msg);
                redraw.store(true, Ordering::Relaxed);
            }
        });

        // Loop until states.quit is false

        while !states.quit {
            // Listen for input events
            if let Ok(Some(ev)) = ctx.input_hnd.read_event() {
                // Pass event to view
                let msg = myview.on(ev);
                states.redraw.store(true, Ordering::Relaxed);
                // Call the elm-like update
                update(&mut states, &mut myview, msg);
            }
            // If redraw, draw interface
            if states.redraw.load(Ordering::Relaxed) {
                let mut chat_log_props = myview.get_props(CHAT_LOG).unwrap();
                chat_log_props.texts.table = Some(
                    states
                        .messages
                        .lock()
                        .unwrap()
                        .iter()
                        .rev()
                        .map(|msg| {
                            vec![
                                TextSpan::from(format!("{}: ", msg.sender)),
                                TextSpan::from(msg.msg.as_str()),
                            ]
                        })
                        .collect(),
                );
                myview.update(CHAT_LOG, chat_log_props).unwrap();

                // Call the elm elm-like vie1 function
                view(&mut ctx, &myview);
                states.redraw.store(false, Ordering::Relaxed);
            }
            sleep(Duration::from_millis(10));
        }

        // Finalize context
        drop(ctx);
    }
}
