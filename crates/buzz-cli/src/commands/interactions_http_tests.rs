//! Exercise actual clap parsing, dispatch, signing and HTTP requests. The remote
//! fixture returns known signed states; relay state-machine behavior is tested
//! separately through the DB's acceptance seam.
use super::*;
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use nostr::Keys;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Parser)]
struct Command {
    #[command(subcommand)]
    cmd: InteractionsCmd,
}

struct Bridge {
    relay: Keys,
    enabled: AtomicBool,
    closed: AtomicBool,
    foreign_state: AtomicBool,
    posted: Mutex<Vec<Event>>,
    filters: Mutex<Vec<Value>>,
}

async fn info(State(s): State<Arc<Bridge>>) -> Json<Value> {
    Json(
        json!({"self":s.relay.public_key().to_hex(),"supported_extensions":if s.enabled.load(Ordering::SeqCst) {vec![EXTENSION]} else {vec![]}}),
    )
}

async fn submit(State(s): State<Arc<Bridge>>, Json(event): Json<Event>) -> Json<Value> {
    event.verify().unwrap();
    let id = event.id.to_hex();
    if event.kind.as_u16() as u32 == KIND_INTERACTION_RESPONSE
        && event
            .tags
            .iter()
            .any(|t| t.as_slice() == ["choice", "approve"])
    {
        s.closed.store(true, Ordering::SeqCst);
    }
    s.posted.lock().unwrap().push(event);
    Json(json!({"event_id":id,"accepted":true,"message":""}))
}

async fn query(State(s): State<Arc<Bridge>>, Json(filters): Json<Vec<Value>>) -> Json<Vec<Event>> {
    assert_eq!(filters.len(), 1);
    let filter = filters[0].clone();
    s.filters.lock().unwrap().push(filter.clone());
    let posted = s.posted.lock().unwrap();
    let Some(prompt) = posted
        .iter()
        .find(|e| e.kind.as_u16() as u32 == KIND_INTERACTION_PROMPT)
    else {
        return Json(vec![]);
    };
    if filter["kinds"][0] == KIND_INTERACTION_PROMPT {
        assert_eq!(filter["ids"][0], prompt.id.to_hex());
        return Json(vec![prompt.clone()]);
    }
    assert_eq!(filter["kinds"][0], KIND_INTERACTION_STATE);
    assert_eq!(filter["authors"][0], s.relay.public_key().to_hex());
    assert_eq!(filter["#d"][0], prompt.id.to_hex());
    let channel = buzz_core::interaction::channel(prompt).unwrap().to_string();
    assert_eq!(filter["#h"][0], channel);
    let closed = s.closed.load(Ordering::SeqCst);
    let foreign = Keys::generate();
    // The client's own recorded answer, so a same-second edit must advance created_at.
    let mine = posted
        .iter()
        .filter(|e| e.kind.as_u16() as u32 == KIND_INTERACTION_RESPONSE)
        .last()
        .map(|e| json!({"pubkey": e.pubkey.to_hex(), "event_id": e.id.to_hex(), "created_at": e.created_at.as_secs(), "choices": []}));
    let event = EventBuilder::new(Kind::Custom(KIND_INTERACTION_STATE as u16),json!({"version":1,"revision":if closed {1} else {0},"status":if closed {"closed"} else {"open"},"close_reason":if closed {Some("first")} else {None},"winner":if closed {Some("approve")} else {None},"tally":{"approve":if closed {1} else {0},"deny":0},"responders":mine.into_iter().collect::<Vec<_>>()}).to_string())
        .tags([pair("d",prompt.id).unwrap(),pair("h",channel).unwrap()])
        .sign_with_keys(if s.foreign_state.load(Ordering::SeqCst) { &foreign } else { &s.relay }).unwrap();
    Json(vec![event])
}

#[tokio::test]
async fn interactions_dispatch_round_trip_and_capability_failure() {
    let state = Arc::new(Bridge {
        relay: Keys::generate(),
        enabled: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        foreign_state: AtomicBool::new(false),
        posted: Mutex::new(vec![]),
        filters: Mutex::new(vec![]),
    });
    let app = Router::new()
        .route("/info", get(info))
        .route("/events", post(submit))
        .route("/query", post(query))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = BuzzClient::new(url, Keys::generate(), None, None).unwrap();
    let channel = uuid::Uuid::new_v4().to_string();
    let ask = || {
        Command::try_parse_from([
            "interactions",
            "ask",
            "--channel",
            &channel,
            "--text",
            "Render E001?",
            "--option",
            "approve:Approve:primary",
            "--option",
            "deny:Deny:danger",
            "--expires",
            "1h",
        ])
        .unwrap()
        .cmd
    };
    assert!(dispatch(ask(), &client).await.is_err());
    assert!(state.posted.lock().unwrap().is_empty());
    state.enabled.store(true, Ordering::SeqCst);
    dispatch(ask(), &client).await.unwrap();
    let prompt = state.posted.lock().unwrap()[0].clone();
    assert_eq!(prompt.pubkey, client.keys().public_key());
    assert_eq!(Prompt::parse(&prompt).unwrap().options.len(), 2);
    let id = prompt.id.to_hex();
    let answer = Command::try_parse_from([
        "interactions",
        "answer",
        "--prompt",
        &id,
        "--choice",
        "approve",
        "--comment",
        "Ready",
    ])
    .unwrap()
    .cmd;
    dispatch(answer, &client).await.unwrap();
    let response = state.posted.lock().unwrap()[1].clone();
    assert_eq!(response.pubkey, client.keys().public_key());
    assert_eq!(response.content, "Ready");
    assert_eq!(
        buzz_core::interaction::prompt_id(&response).unwrap(),
        prompt.id
    );
    dispatch(InteractionsCmd::Get { prompt: id.clone() }, &client)
        .await
        .unwrap();
    dispatch(
        InteractionsCmd::Wait {
            prompt: id.clone(),
            timeout: "1s".into(),
        },
        &client,
    )
    .await
    .unwrap();
    state.foreign_state.store(true, Ordering::SeqCst);
    assert!(
        dispatch(InteractionsCmd::Get { prompt: id.clone() }, &client)
            .await
            .is_err()
    );
    state.foreign_state.store(false, Ordering::SeqCst);
    state.closed.store(false, Ordering::SeqCst);
    assert!(matches!(
        dispatch(
            InteractionsCmd::Wait {
                prompt: id,
                timeout: "1s".into()
            },
            &client
        )
        .await,
        Err(CliError::Other(_))
    ));
    server.abort();
}

fn bridge() -> Arc<Bridge> {
    Arc::new(Bridge {
        relay: Keys::generate(),
        enabled: AtomicBool::new(true),
        closed: AtomicBool::new(false),
        foreign_state: AtomicBool::new(false),
        posted: Mutex::new(vec![]),
        filters: Mutex::new(vec![]),
    })
}

async fn serve(state: Arc<Bridge>) -> (BuzzClient, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/info", get(info))
        .route("/events", post(submit))
        .route("/query", post(query))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        BuzzClient::new(url, Keys::generate(), None, None).unwrap(),
        server,
    )
}

fn parse(args: &[&str]) -> Result<InteractionsCmd, clap::Error> {
    Command::try_parse_from(std::iter::once("interactions").chain(args.iter().copied()))
        .map(|c| c.cmd)
}

#[tokio::test]
async fn polls_forms_and_close_round_trip_through_the_bridge() {
    let state = bridge();
    let (client, server) = serve(state.clone()).await;
    let channel = uuid::Uuid::new_v4().to_string();
    // A poll accepts multi-select bounds and defaults to closing on expiry.
    let poll = parse(&[
        "poll",
        "--channel",
        &channel,
        "--text",
        "Which thumbnail?",
        "--option",
        "1:A",
        "--option",
        "2:B",
        "--option",
        "3:C",
        "--min",
        "1",
        "--max",
        "2",
        "--expires",
        "2h",
    ])
    .unwrap();
    dispatch(poll, &client).await.unwrap();
    let prompt = state.posted.lock().unwrap()[0].clone();
    let schema = Prompt::parse(&prompt).unwrap();
    assert_eq!(schema.itype, "poll");
    assert_eq!(schema.closes, "expiry");
    assert_eq!((schema.min, schema.max), (1, 2));
    assert!(schema.deadline > Timestamp::now().as_secs() + 7000);
    let id = prompt.id.to_hex();
    // Two choices ride on one signed response.
    dispatch(
        parse(&["answer", "--prompt", &id, "--choice", "1", "--choice", "3"]).unwrap(),
        &client,
    )
    .await
    .unwrap();
    let response = state.posted.lock().unwrap()[1].clone();
    assert_eq!(
        schema.answer(&response).unwrap().choices,
        ["1", "3"].into_iter().map(String::from).collect()
    );
    // A same-second revision must strictly advance created_at past the recorded answer.
    dispatch(
        parse(&["answer", "--prompt", &id, "--choice", "2"]).unwrap(),
        &client,
    )
    .await
    .unwrap();
    let revised = state.posted.lock().unwrap()[2].clone();
    assert!(revised.created_at > response.created_at);
    // Local validation rejects an impossible answer before any HTTP write.
    let before = state.posted.lock().unwrap().len();
    assert!(matches!(
        dispatch(
            parse(&["answer", "--prompt", &id, "--choice", "1", "--choice", "2", "--choice", "3"])
                .unwrap(),
            &client,
        )
        .await,
        Err(CliError::Usage(_))
    ));
    assert!(matches!(
        dispatch(
            parse(&["answer", "--prompt", &id, "--value", "novalue"]).unwrap(),
            &client,
        )
        .await,
        Err(CliError::Usage(_))
    ));
    assert_eq!(state.posted.lock().unwrap().len(), before);
    // Close names the signed prompt with the prompt marker.
    dispatch(parse(&["close", "--prompt", &id]).unwrap(), &client)
        .await
        .unwrap();
    let close = state.posted.lock().unwrap().last().cloned().unwrap();
    assert_eq!(close.kind.as_u16() as u32, KIND_INTERACTION_CLOSE);
    assert_eq!(
        buzz_core::interaction::prompt_id(&close).unwrap(),
        prompt.id
    );
    assert_eq!(
        buzz_core::interaction::channel(&close).unwrap().to_string(),
        channel
    );
    server.abort();
}

#[tokio::test]
async fn forms_send_typed_values_and_json_envelopes_are_bounded() {
    let state = bridge();
    let (client, server) = serve(state.clone()).await;
    let channel = uuid::Uuid::new_v4().to_string();
    dispatch(
        parse(&[
            "ask",
            "--channel",
            &channel,
            "--text",
            "Episode details",
            "--type",
            "form",
            "--field",
            "title:Title:text:required",
            "--field",
            "length:Length:select:required",
            "--select",
            "length=60s,6min",
            "--closes",
            "manual",
            "--responders",
            "listed",
            "--responder",
            &Keys::generate().public_key().to_hex(),
        ])
        .unwrap(),
        &client,
    )
    .await
    .unwrap();
    let prompt = state.posted.lock().unwrap()[0].clone();
    let schema = Prompt::parse(&prompt).unwrap();
    assert_eq!(schema.itype, "form");
    assert_eq!(schema.responders, "listed");
    assert_eq!(schema.listed.len(), 1);
    assert_eq!(schema.fields[1].options.len(), 2);
    let id = prompt.id.to_hex();
    dispatch(
        parse(&[
            "answer",
            "--prompt",
            &id,
            "--value",
            "title=The Door",
            "--value",
            "length=6min",
        ])
        .unwrap(),
        &client,
    )
    .await
    .unwrap();
    let response = state.posted.lock().unwrap()[1].clone();
    let answer = schema.answer(&response).unwrap();
    assert_eq!(answer.values["title"], "The Door");
    assert_eq!(answer.values["length"], "6min");
    // Missing required fields fail locally.
    assert!(matches!(
        dispatch(
            parse(&["answer", "--prompt", &id, "--value", "title=x"]).unwrap(),
            &client
        )
        .await,
        Err(CliError::Usage(_))
    ));
    // JSON envelopes: inline, from a file, and bounded at 64 KiB.
    let deadline = Timestamp::now().as_secs() + 600;
    let envelope = json!({"content":"Ship it?","tags":[["h",channel],["itype","buttons"],["opt","go","Go: now"],["closes","first"],["deadline",deadline.to_string()]]});
    dispatch(
        parse(&["ask", "--json", &envelope.to_string()]).unwrap(),
        &client,
    )
    .await
    .unwrap();
    let inline = state.posted.lock().unwrap().last().cloned().unwrap();
    assert_eq!(Prompt::parse(&inline).unwrap().options[0].label, "Go: now");
    let dir = std::env::temp_dir().join(format!("buzz-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("prompt.json");
    std::fs::write(&path, envelope.to_string()).unwrap();
    dispatch(
        parse(&["ask", "--json", &format!("@{}", path.display())]).unwrap(),
        &client,
    )
    .await
    .unwrap();
    let from_file = state.posted.lock().unwrap().last().cloned().unwrap();
    assert_eq!(from_file.content, "Ship it?");
    let big = dir.join("big.json");
    std::fs::write(
        &big,
        format!("{{\"content\":\"{}\",\"tags\":[]}}", "x".repeat(70_000)),
    )
    .unwrap();
    assert!(matches!(
        dispatch(
            parse(&["ask", "--json", &format!("@{}", big.display())]).unwrap(),
            &client
        )
        .await,
        Err(CliError::Usage(_))
    ));
    let unknown = json!({"content":"q","tags":[],"kind":1}).to_string();
    assert!(matches!(
        dispatch(parse(&["ask", "--json", &unknown]).unwrap(), &client).await,
        Err(CliError::Usage(_))
    ));
    // A poll envelope must declare its own itype.
    assert!(matches!(
        dispatch(
            parse(&["poll", "--json", &envelope.to_string()]).unwrap(),
            &client
        )
        .await,
        Err(CliError::Usage(_))
    ));
    std::fs::remove_dir_all(&dir).unwrap();
    server.abort();
}

#[test]
fn ask_flags_are_parsed_strictly() {
    assert!(
        parse(&["ask", "--text", "q"]).is_err(),
        "channel is required"
    );
    assert!(
        parse(&["ask", "--channel", "c"]).is_err(),
        "text is required"
    );
    assert!(
        parse(&["ask", "--json", "{}", "--channel", "c"]).is_err(),
        "json conflicts with schema flags"
    );
    assert!(parse(&["ask", "--json", "{}"]).is_ok());
    assert!(parse(&["wait", "--prompt", "x", "--timeout", "5m"]).is_ok());
    assert!(parse(&["answer"]).is_err(), "prompt is required");
}

#[tokio::test]
async fn malformed_schema_flags_fail_before_any_network_call() {
    let client =
        BuzzClient::new("http://127.0.0.1:9".into(), Keys::generate(), None, None).unwrap();
    let channel = uuid::Uuid::new_v4().to_string();
    for (name, args) in [
        ("option without label", vec!["--option", "approve"]),
        ("field with three parts", vec!["--field", "a:b:c"]),
        ("select without equals", vec!["--select", "length"]),
        ("responder not a key", vec!["--responder", "nope"]),
        ("zero expiry", vec!["--expires", "0s"]),
        ("expiry over 30 days", vec!["--expires", "31d"]),
        ("bad reply parent", vec!["--reply-to", "zz"]),
    ] {
        let mut full = vec![
            "ask",
            "--channel",
            channel.as_str(),
            "--text",
            "q",
            "--option",
            "a:A",
        ];
        full.extend(args);
        let InteractionsCmd::Ask(parsed) = parse(&full).unwrap() else {
            panic!("{name}: not an ask");
        };
        let error = ask_builder(&client, parsed, false).await.err();
        assert!(
            matches!(error, Some(CliError::Usage(_))),
            "{name}: {error:?}"
        );
    }
    let InteractionsCmd::Ask(parsed) = parse(&[
        "ask",
        "--channel",
        "not-a-uuid",
        "--text",
        "q",
        "--option",
        "a:A",
    ])
    .unwrap() else {
        panic!("not an ask");
    };
    assert!(matches!(
        ask_builder(&client, parsed, false).await,
        Err(CliError::Usage(_))
    ));
}
