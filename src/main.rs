use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures::{sink::SinkExt, stream::StreamExt};
use num_bigint::BigInt;
use num_traits::One;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::time::{interval, Duration};
use std::path::Path;

type SharedState = Arc<Mutex<BigInt>>;

const SAVE_FILE: &str = "the_number.txt";

async fn load_state() -> BigInt {
    match tokio::fs::read_to_string(SAVE_FILE).await {
        Ok(content) => {
            if let Ok(num) = content.trim().parse::<BigInt>() {
                println!("Restored number: {}", num);
                return num;
            }
        }
        Err(_) => println!("No saved state found, starting from 0"),
    }
    BigInt::from(0)
}

async fn save_state(state: SharedState) {
    let num_str = state.lock().unwrap().to_string();
    if let Err(e) = tokio::fs::write(SAVE_FILE, &num_str).await {
        eprintln!("Failed to save state: {}", e);
    }
}

async fn periodic_save_task(state: SharedState) {
    let mut save_interval = interval(Duration::from_secs(5));
    loop {
        save_interval.tick().await;
        save_state(state.clone()).await;
    }
}

#[tokio::main]
async fn main() {
    let initial_value = load_state().await;
    let state = Arc::new(Mutex::new(initial_value));
    let (tx, _) = broadcast::channel::<String>(1000);
    let tx = Arc::new(tx);

    let state_clone = state.clone();
    tokio::spawn(async move {
        periodic_save_task(state_clone).await;
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_handler))
        .with_state((state, tx));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Server running on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../index.html"))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    axum::extract::State((state, tx)): axum::extract::State<(SharedState, Arc<broadcast::Sender<String>>)>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, tx))
}

async fn handle_socket(socket: WebSocket, state: SharedState, tx: Arc<broadcast::Sender<String>>) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = tx.subscribe();

    let current = state.lock().unwrap().to_string();
    if sender.send(Message::Text(current)).await.is_err() {
        return;
    }

    let mut send_task = tokio::spawn(async move {
        while let Ok(value) = rx.recv().await {
            if sender.send(Message::Text(value)).await.is_err() {
                break;
            }
        }
    });

    let state_clone = state.clone();
    let tx_clone = tx.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            if let Message::Text(text) = msg {
                if text != "+" && text != "-" {
                    continue;
                }

                let new_value = {
                    let mut num = state_clone.lock().unwrap();
                    if text == "+" {
                        *num += BigInt::one();
                    } else {
                        *num -= BigInt::one();
                    }
                    num.to_string()
                };
                let _ = tx_clone.send(new_value);
            }
        }
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    }
}