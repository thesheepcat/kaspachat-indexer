#![allow(warnings)]

use async_channel::{Receiver, Sender};
use axum::{
    extract,
    extract::{Path, Query, State},
    handler,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use futures::stream::Collect;
pub use futures::{select, select_biased, FutureExt, Stream, StreamExt, TryStreamExt};
use kaspa_notify::notification::test_helpers::Data;
use polodb_core::bson::{doc, Document};
use polodb_core::{Collection, CollectionT, Database};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::{any::Any, fmt::format};
use std::{
    clone,
    sync::atomic::{AtomicBool, Ordering},
};

// We use workflow-rs primitives for async task and channel management
// as they function uniformly in tokio as well as WASM32 runtimes.
use workflow_core::channel::{oneshot, Channel, DuplexChannel};
use workflow_core::task::spawn;
use workflow_log::prelude::*;

// Kaspa RPC primitives
use kaspa_wrpc_client::prelude::*;
// reuse wRPC Result type for convenience
use kaspa_wrpc_client::result::Result;

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    sender: String,
    receiver: String,
    transaction_id: String,
    block_time: u64,
    message: String,
}

impl Message {
    fn new(
        sender: String,
        receiver: String,
        transaction_id: String,
        block_time: u64,
        message: String,
    ) -> Self {
        Self {
            sender,
            receiver,
            transaction_id,
            block_time,
            message,
        }
    }
}

struct Inner {
    // task control duplex channel - a pair of channels where sender
    // is used to signal an async task termination request and receiver
    // is used to signal task termination completion.
    task_ctl: DuplexChannel<()>,
    // Kaspa wRPC client instance
    client: Arc<KaspaRpcClient>,
    // our own view on the connection state
    is_connected: AtomicBool,
    // channel supplied to the notification subsystem
    // to receive the node notifications we subscribe to
    notification_channel: Channel<Notification>,
    // listener id used to manage notification scopes
    // we can have multiple IDs for different scopes
    // paired with multiple notification channels
    listener_id: Mutex<Option<ListenerId>>,
    stored_messages: Collection<Message>,
}

// Example primitive that manages an RPC connection and
// runs its own event task to handle RPC connection
// events and node notifications we subscribe to.
#[derive(Clone)]
pub struct Listener {
    inner: Arc<Inner>,
}

impl Listener {
    pub fn try_new(
        network_id: NetworkId,
        url: Option<String>,
        stored_messages: Collection<Message>,
    ) -> Result<Self> {
        // if not url is supplied we use the default resolver to
        // obtain the public node rpc endpoint
        let (resolver, url) = if let Some(url) = url {
            (None, Some(url))
        } else {
            (Some(Resolver::default()), None)
        };

        // Create a basic Kaspa RPC client instance using Borsh encoding.
        let client = Arc::new(KaspaRpcClient::new_with_args(
            WrpcEncoding::Borsh,
            url.as_deref(),
            resolver,
            Some(network_id),
            None,
        )?);

        let inner = Inner {
            task_ctl: DuplexChannel::oneshot(),
            client,
            is_connected: AtomicBool::new(false),
            notification_channel: Channel::unbounded(),
            listener_id: Mutex::new(None),
            stored_messages: stored_messages,
        };

        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    // Helper fn to check if we are currently connected
    // to the node. This only represents our own view of
    // the connection state (i.e. if in a different setup
    // our event task is shutdown, the RPC client may remain
    // connected.
    fn is_connected(&self) -> bool {
        self.inner.is_connected.load(Ordering::SeqCst)
    }

    // Start the listener
    async fn start(&self) -> Result<()> {
        // we do not block the async connect() function
        // as we handle the connection state in the event task
        let options = ConnectOptions {
            block_async_connect: false,
            ..Default::default()
        };

        // start the event processing task
        self.start_event_task().await?;

        // start the RPC connection. this will initiate an RPC connection background task that will
        // continuously try to connect to the given URL or query a URL from the resolver if one is provided.
        self.client().connect(Some(options)).await?;

        Ok(())
    }

    // Stop the listener
    async fn stop(&self) -> Result<()> {
        // Disconnect the RPC client
        self.client().disconnect().await?;
        // make sure to stop the event task after the RPC client is disconnected to receive and handle disconnection events.
        self.stop_event_task().await?;
        Ok(())
    }

    pub fn client(&self) -> &Arc<KaspaRpcClient> {
        &self.inner.client
    }

    async fn register_notification_listeners(&self) -> Result<()> {
        // IMPORTANT: notification scopes are managed by the node
        // for the lifetime of the RPC connection, as such they
        // are "lost" if we disconnect. For that reason we must
        // re-register all notification scopes when we connect.

        let listener_id = self
            .client()
            .rpc_api()
            .register_new_listener(ChannelConnection::new(
                "wrpc-example-subscriber",
                self.inner.notification_channel.sender.clone(),
                ChannelType::Persistent,
            ));
        *self.inner.listener_id.lock().unwrap() = Some(listener_id);
        self.client()
            .rpc_api()
            .start_notify(listener_id, Scope::BlockAdded(BlockAddedScope {}))
            .await?;
        Ok(())
    }

    async fn unregister_notification_listener(&self) -> Result<()> {
        let listener_id = self.inner.listener_id.lock().unwrap().take();
        if let Some(id) = listener_id {
            // We do not need to unregister previously registered
            // notifications as we are unregistering the entire listener.

            // If we do want to unregister individual notifications we can do:
            // `self.client().rpc_api().stop_notify(listener_id, Scope:: ... ).await?;`
            // for each previously registered notification scope.

            self.client().rpc_api().unregister_listener(id).await?;
        }
        Ok(())
    }

    // generic notification handler fn called by the event task
    async fn handle_notification(&self, notification: Notification) -> Result<()> {
        //log_info!("Notification: {notification:?}");
        let stored_messages = &self.inner.stored_messages;
        match notification {
            Notification::BlockAdded(block_notification) => {
                extract_relevant_transactions_data(block_notification, stored_messages)
            }
            _ => (),
        }

        fn extract_relevant_transactions_data(
            block_notification: BlockAddedNotification,
            stored_messages: &Collection<Message>,
        ) {
            for transaction in &block_notification.block.transactions {
                if transaction.payload.len() >= 4 {
                    //Payload conversion rule:
                    // From text to HEX:
                    //     From 0123456789 to 30313233343536373839
                    // Then, from HEX to uint8:
                    //     from 30313233343536373839 to 48 49 50 51 52 53 54 55 56 57
                    // KaspaChat protocol prefix (kch:) in uint8: 107 99 104 58
                    // KaspaTalk new protocol prefix (ktk:) in uint8: 107 116 107 58
                    let KASPATALK_PROTOCOL_PREFIX = [107, 116, 107, 58];
                    if (transaction.payload[0] == KASPATALK_PROTOCOL_PREFIX[0])
                        && (transaction.payload[1] == KASPATALK_PROTOCOL_PREFIX[1])
                        && (transaction.payload[2] == KASPATALK_PROTOCOL_PREFIX[2])
                        && (transaction.payload[3] == KASPATALK_PROTOCOL_PREFIX[3])
                    {
                        //log_info!("MESSAGE RECEIVED!");

                        // Extracting receiver address
                        let receiver_address: String = match &transaction.outputs[0].verbose_data {
                            Some(x) => x.script_public_key_address.address_to_string(),
                            None => "".to_owned(),
                        };

                        // Extracting transaction ID
                        let transaction_id: String = match &transaction.verbose_data {
                            Some(x) => x.transaction_id.to_string(),
                            None => "".to_owned(),
                        };

                        // Extracting block_time
                        let block_time: u64 = match &transaction.verbose_data {
                            Some(x) => x.block_time,
                            None => 0,
                        };

                        // Extracting payload
                        let payload: String = std::str::from_utf8(&transaction.payload)
                            .expect("None")
                            .to_owned();

                        // Extracting message from payload
                        let clean_payload: String = payload[4..payload.len()].to_string();

                        // Split the string at the first occurrence of '|'
                        let payload_parts: Vec<&str> = clean_payload.splitn(2, '|').collect();

                        // Extracting sender address
                        // let sender_address: String = "third_sender_address".to_owned();

                        let mut sender_address = "".to_owned();
                        let mut message = "".to_owned();

                        // Check if we have exactly two parts #TODO
                        if payload_parts.len() == 2 {
                            sender_address = String::from(payload_parts[0]);
                            message = String::from(payload_parts[1]);
                        } else {
                            println!("Wrong payload");
                            return;
                        }

                        // Check if transaction is already stored; if not, save transaction in database
                        match stored_messages
                            .find_one(doc! {"transaction_id":  { "$eq": &transaction_id }})
                        {
                            Ok(stored_message) => match stored_message {
                                Some(stored_message) => {
                                    /* 
                                    log_info!(
                                        "Transaction found in db: {}",
                                        stored_message.transaction_id
                                    );
                                    */
                                    //log_info!("MESSAGE SKIPPED!");
                                }
                                None => {
                                    //log_info!("Transaction not found in db: {}", &transaction_id);
                                    let message_to_save = Message::new(
                                        sender_address,
                                        receiver_address,
                                        transaction_id,
                                        block_time,
                                        message,
                                    );
                                    stored_messages.insert_one(message_to_save);
                                    log_info!("MESSAGE SAVED!");
                                }
                            },
                            Err(_) => {}
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // generic connection handler fn called by the event task
    async fn handle_connect(&self) -> Result<()> {
        //println!("Connected to {:?}", self.client().url());

        // make an RPC method call to the node...
        let server_info = self.client().get_server_info().await?;
        log_info!("Server info: {server_info:?}");

        // now that we have successfully connected we
        // can register for notifications
        self.register_notification_listeners().await?;

        // store internal state indicating that we are currently connected
        self.inner.is_connected.store(true, Ordering::SeqCst);
        Ok(())
    }

    // generic disconnection handler fn called by the event task
    async fn handle_disconnect(&self) -> Result<()> {
        println!("Disconnected from {:?}", self.client().url());

        // Unregister notifications
        self.unregister_notification_listener().await?;

        // store internal state indicating that we are currently disconnected
        self.inner.is_connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn start_event_task(&self) -> Result<()> {
        // clone self for the async task
        let listener = self.clone();

        // clone the "rpc control channel" that posts notifications
        // when the RPC channel is connected or disconnected
        let rpc_ctl_channel = self.client().rpc_ctl().multiplexer().channel();

        // clone our sender and receiver channels for task control
        // these are obtained from the `DuplexChannel` - a pair of
        // channels where sender acts as a trigger signaling termination
        // and the receiver is used to signal termination completion.
        // (this is a common pattern used for channel lifetime management
        // in the rusty kaspa framework)
        let task_ctl_receiver = self.inner.task_ctl.request.receiver.clone();
        let task_ctl_sender = self.inner.task_ctl.response.sender.clone();

        // clone notification event channel that we provide to the RPC client
        // notification subsystem to receive notifications from the node.
        let notification_receiver = self.inner.notification_channel.receiver.clone();

        spawn(async move {
            loop {
                select_biased! {
                    msg = rpc_ctl_channel.receiver.recv().fuse() => {
                        match msg {
                            Ok(msg) => {

                                // handle RPC channel connection and disconnection events
                                match msg {
                                    RpcState::Connected => {
                                        if let Err(err) = listener.handle_connect().await {
                                            log_error!("Error in connect handler: {err}");
                                        }
                                    },
                                    RpcState::Disconnected => {
                                        if let Err(err) = listener.handle_disconnect().await {
                                            log_error!("Error in disconnect handler: {err}");
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                // this will never occur if the RpcClient is owned and
                                // properly managed. This can only occur if RpcClient is
                                // deleted while this task is still running.
                                log_error!("RPC CTL channel error: {err}");
                                panic!("Unexpected: RPC CTL channel closed, halting...");
                            }
                        }
                    }
                    notification = notification_receiver.recv().fuse() => {
                        match notification {
                            Ok(notification) => {
                                if let Err(err) = listener.handle_notification(notification).await {
                                    log_error!("Error while handling notification: {err}");
                                }
                            }
                            Err(err) => {
                                panic!("RPC notification channel error: {err}");
                            }
                        }
                    },

                    // we use select_biased to drain rpc_ctl
                    // and notifications before shutting down
                    // as such task_ctl is last in the poll order
                    _ = task_ctl_receiver.recv().fuse() => {
                        break;
                    },

                }
            }

            log_info!("Event task existing...");

            // handle our own power down on the rpc channel that remains connected
            if listener.is_connected() {
                listener
                    .handle_disconnect()
                    .await
                    .unwrap_or_else(|err| log_error!("{err}"));
            }

            // post task termination event
            task_ctl_sender.send(()).await.unwrap();
        });
        Ok(())
    }

    async fn stop_event_task(&self) -> Result<()> {
        self.inner
            .task_ctl
            .signal(())
            .await
            .expect("stop_event_task() signal error");
        Ok(())
    }
}

struct AppState {
    database: Database,
}

// REST API call to retrieve all messages in database
#[axum_macros::debug_handler]
async fn get_all_messages(State(state): State<Arc<AppState>>) -> Json<Vec<Message>> {
    let mut all_messages: Vec<Message> = Vec::new();
    let stored_messages: Collection<Message> = state.database.collection("stored-messages");
    let query_result = stored_messages
        .find(doc! {})
        .run()
        .expect("Error while querying database");
    for item in query_result {
        match item {
            Ok(message) => all_messages.push(message),
            Err(error) => println!("Error while querying data: {error:?}"),
        };
    }
    Json(all_messages)
}

#[derive(Deserialize, Clone)]
struct GetMessagesQueryParameters {
    address_1: String,
    address_2: String,
}

// REST API call to retrieve messages of a specific conversation (http://localhost:3000/get-messages?address_1=abc&address_2=def)
// http://localhost:3000/get-messages?address_1=sender_address&address_2=kaspatest:qr3qzzsx6wc59tgchvx2eyrms4u76tcu0xdncz2a4zd83ddnwrl95e8v3racu
#[axum_macros::debug_handler]
async fn get_messages(
    State(state): State<Arc<AppState>>,
    parameters: Query<GetMessagesQueryParameters>,
) -> Json<Vec<Message>> {
    let address_1: String = parameters.address_1.clone();
    let address_2: String = parameters.address_2.clone();
    let mut message_list: Vec<Message> = Vec::new();
    let stored_messages: Collection<Message> = state.database.collection("stored-messages");
    let query_result = stored_messages
        .find(doc! {
            "$or": [
                {
                    "sender" : { "$eq" : address_1.clone()},
                    "receiver" : { "$eq" : address_2.clone()}
                },
                {
                    "sender" : { "$eq" : address_2.clone()},
                    "receiver" : { "$eq" : address_1.clone()}
                },
            ]
        })
        .run()
        .expect("Error while querying database");
    for item in query_result {
        match item {
            Ok(message) => message_list.push(message),
            Err(error) => println!("Error while querying data: {error:?}"),
        };
    }
    Json(message_list)
}

#[derive(Deserialize, Clone)]
struct GetPeersQueryParameters {
    address: String,
}

// REST API call to retrieve peers who have active conversation with a specific address (http://localhost:3000/get-peers?address=abc)
// http://localhost:3000/get-peers?address=kaspatest:qr3qzzsx6wc59tgchvx2eyrms4u76tcu0xdncz2a4zd83ddnwrl95e8v3racu
#[axum_macros::debug_handler]
async fn get_peers(
    State(state): State<Arc<AppState>>,
    parameters: Query<GetPeersQueryParameters>,
) -> Json<Vec<String>> {
    let user_address: String = parameters.address.clone();
    let mut peers_list: Vec<String> = Vec::new();
    let stored_messages: Collection<Message> = state.database.collection("stored-messages");
    let query_result = stored_messages
        .find(doc! {
            "$or": [
                {"sender" : { "$eq" : user_address.clone()}},
                {"receiver" : { "$eq" : user_address.clone()}}
            ]
        })
        .run()
        .expect("Error while querying database");
    for item in query_result {
        match item {
            Ok(message) => {
                if (message.sender == user_address.clone()) {
                    if !peers_list.contains(&message.receiver) {
                        peers_list.push(message.receiver);
                    }
                } else if (message.sender != user_address.clone()) {
                    if !peers_list.contains(&message.sender) {
                        peers_list.push(message.sender);
                    }
                }
            }
            Err(error) => println!("Error while querying data: {error:?}"),
        };
    }
    Json(peers_list)
}

#[tokio::main]
async fn main() -> Result<()> {
    /// Parsing CLI flags
    #[derive(Parser, Debug)]
    #[command(version, about, long_about = None)]
    struct Args {
        #[arg(short, long)]
        rusty_kaspa_address: String,
    }
    let args = Args::parse();

    // Building database
    let db = Database::open_path("kaspatalk.db").unwrap();
    let stored_messages: Collection<Message> = db.collection("stored-messages");

    // Building rusty-kaspa listener
    let rusty_kaspa_address_prefix = "ws://".to_owned();
    let rusty_kaspa_url = args.rusty_kaspa_address;
    let complete_rusty_kaspa_address = format!("{rusty_kaspa_address_prefix}{rusty_kaspa_url}");
    let listener = Listener::try_new(
        NetworkId::with_suffix(NetworkType::Testnet, 11),
        Some(complete_rusty_kaspa_address),
        stored_messages,
    )?;
    listener.start().await?;

    // Building webserver app
    let app_state = Arc::new(AppState { database: db });
    let router = Router::new()
        .route("/get-all-messages", get(get_all_messages))
        .route("/get-messages", get(get_messages))
        .route("/get-peers", get(get_peers))
        .with_state(app_state);

    // Serving webserver app
    let webserver = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(webserver, router).await.unwrap();

    // Shutdown procedure
    let (shutdown_sender, shutdown_receiver) = oneshot::<()>();
    ctrlc::set_handler(move || {
        log_info!("^SIGTERM - shutting down...");
        shutdown_sender
            .try_send(())
            .expect("Error sending shutdown signal...");
    })
    .expect("Unable to set the Ctrl+C signal handler");
    shutdown_receiver
        .recv()
        .await
        .expect("Error waiting for shutdown signal...");
    listener.stop().await?;
    Ok(())
}
