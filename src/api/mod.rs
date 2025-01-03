use std::sync::Arc;
use axum::extract::{Query, State};
use axum::Json;
use polodb_core::bson::doc;
use polodb_core::{Collection, CollectionT};
use crate::{AppState, GetMessagesQueryParameters};
use crate::message::Message;
use crate::GetPeersQueryParameters::*;

///GET Calls
// REST API call to retrieve messages of a specific conversation (http://localhost:3000/get-messages?address_1=abc&address_2=def)
// http://localhost:3000/get-messages?address_1=sender_address&address_2=kaspatest:qr3qzzsx6wc59tgchvx2eyrms4u76tcu0xdncz2a4zd83ddnwrl95e8v3racu
#[axum_macros::debug_handler]
pub(crate) async fn get_messages(
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


// REST API call to retrieve peers who have active conversation with a specific address (http://localhost:3000/get-peers?address=abc)
// http://localhost:3000/get-peers?address=kaspatest:qr3qzzsx6wc59tgchvx2eyrms4u76tcu0xdncz2a4zd83ddnwrl95e8v3racu
#[axum_macros::debug_handler]
pub(crate) async fn get_peers(
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
// REST API call to retrieve all messages in database
#[axum_macros::debug_handler]
pub(crate) async fn get_all_messages(State(state): State<Arc<AppState>>) -> Json<Vec<Message>> {
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
