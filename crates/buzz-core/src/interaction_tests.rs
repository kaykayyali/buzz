use super::*;
use nostr::Keys;

fn event(kind: u32, content: &str, extra: &[&[&str]], keys: &Keys, time: u64) -> Event {
    let mut ts = vec![Tag::parse(["h", "00000000-0000-0000-0000-000000000001"]).unwrap()];
    ts.extend(extra.iter().map(|t| Tag::parse(t.iter().copied()).unwrap()));
    EventBuilder::new(Kind::Custom(kind as u16), content)
        .tags(ts)
        .custom_created_at(Timestamp::from(time))
        .sign_with_keys(keys)
        .unwrap()
}
fn prompt(extra: &[&[&str]]) -> Event {
    let mut ts = vec![
        &["itype", "buttons"][..],
        &["opt", "approve", "Approve", "primary"],
        &["opt", "deny", "Deny", "danger"],
        &["closes", "first"],
        &["deadline", "2000"],
    ];
    ts.extend_from_slice(extra);
    event(
        KIND_INTERACTION_PROMPT,
        "Render?",
        &ts,
        &Keys::generate(),
        1000,
    )
}
fn response(pk: &Keys, choice: &str, time: u64) -> Event {
    event(
        KIND_INTERACTION_RESPONSE,
        "",
        &[&["e", &"a".repeat(64), "", "prompt"], &["choice", choice]],
        pk,
        time,
    )
}

#[test]
fn schema_rejects_ambiguous_or_unsafe_prompts() {
    assert!(Prompt::parse(&prompt(&[])).is_ok());
    for extra in [
        vec![&["itype", "poll"][..]],
        vec![&["visibility", "asker-only"][..]],
        vec![&["visibility", "tallies-only"][..]],
        vec![&["expiration", "2000"][..]],
        vec![&["field", "password", "Password", "secret", "required"][..]],
        vec![&["opt", "APPROVE", "Other"][..]],
        vec![&["opt", "other", "Approve"][..]],
        vec![&["opt", "other", "deny"][..]],
        vec![&["responders", "listed"][..]],
        vec![&["max", "2"][..]],
        vec![&["via", "forged"][..]],
    ] {
        assert!(
            Prompt::parse(&prompt(&extra)).is_err(),
            "accepted {extra:?}"
        );
    }
    let p = Prompt::parse(&prompt(&[])).unwrap();
    assert!(p.validate_lifetime(1000).is_ok());
    assert!(p.validate_lifetime(2000).is_err());
    assert!(p.text_answer(" \nAPPROVE\t").is_some());
    for text in ["approve!", "approve but revise", "approved", ""] {
        assert!(p.text_answer(text).is_none());
    }
}

#[test]
fn validation_and_replacement_have_one_actor_one_vote() {
    let mut p = Prompt::parse(&prompt(&[])).unwrap();
    p.closes = "manual".into();
    let k = Keys::generate();
    let mut state = InteractionState::default();
    let a = response(&k, "approve", 1000);
    assert!(state
        .respond(&p, a.clone(), p.answer(&a).unwrap(), 1000)
        .unwrap());
    assert!(!state
        .respond(&p, a.clone(), p.answer(&a).unwrap(), 1000)
        .unwrap());
    let b = response(&k, "deny", 1001);
    state
        .respond(&p, b.clone(), p.answer(&b).unwrap(), 1001)
        .unwrap();
    assert_eq!(state.summary(&p)["tally"]["approve"], 0);
    assert_eq!(state.summary(&p)["tally"]["deny"], 1);
    assert_eq!(state.votes.len(), 1);
    assert!(state
        .respond(&p, a.clone(), p.answer(&a).unwrap(), 1002)
        .is_err());
    assert!(p.answer(&response(&k, "maybe", 1001)).is_err());
    state.close("manual");
    assert!(state
        .respond(&p, response(&k, "approve", 1003), Answer::default(), 1003)
        .is_err());
    assert!(!state.close("expiry"));
}

#[test]
fn close_rules_ties_and_deadline_are_deterministic() {
    let p = Prompt::parse(&prompt(&[])).unwrap();
    let a = response(&Keys::generate(), "approve", 1000);
    let b = response(&Keys::generate(), "deny", 1001);
    let mut state = InteractionState::default();
    state
        .respond(&p, a.clone(), p.answer(&a).unwrap(), 1000)
        .unwrap();
    assert_eq!(state.summary(&p)["winner"], "approve");
    assert!(state
        .respond(&p, b.clone(), p.answer(&b).unwrap(), 1001)
        .is_err());
    let mut p = p;
    p.closes = "quorum:2".into();
    let mut state = InteractionState::default();
    state
        .respond(&p, a.clone(), p.answer(&a).unwrap(), 1000)
        .unwrap();
    assert!(state.close_reason.is_none());
    state
        .respond(&p, b.clone(), p.answer(&b).unwrap(), 1001)
        .unwrap();
    assert_eq!(state.close_reason.as_deref(), Some("quorum"));
    assert!(state.summary(&p)["winner"].is_null());
    assert!(InteractionState::default()
        .respond(&p, a.clone(), p.answer(&a).unwrap(), 2000)
        .is_err());
}

#[test]
fn forms_validate_real_values_and_required_fields() {
    let k = Keys::generate();
    let ev = event(
        KIND_INTERACTION_PROMPT,
        "Details?",
        &[
            &["itype", "form"],
            &["closes", "first"],
            &["deadline", "2000"],
            &["field", "amount", "Amount", "number", "required"],
            &["field", "date", "Date", "date", "optional"],
            &["field", "ready", "Ready", "boolean", "optional"],
            &["field", "size", "Size", "select", "optional"],
            &["optsel", "size", "small", "Small"],
        ],
        &k,
        1000,
    );
    let p = Prompt::parse(&ev).unwrap();
    let make = |values: &[&[&str]]| {
        let id = ev.id.to_hex();
        let mut ts = vec![vec!["e", id.as_str(), "", "prompt"]];
        ts.extend(values.iter().map(|t| t.to_vec()));
        event(
            KIND_INTERACTION_RESPONSE,
            "",
            &ts.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            &k,
            1000,
        )
    };
    assert!(p
        .answer(&make(&[
            &["value", "amount", "12.5"],
            &["value", "date", "2026-09-08"],
            &["value", "ready", "false"],
            &["value", "size", "small"]
        ]))
        .is_ok());
    for values in [
        vec![],
        vec![&["value", "amount", "NaN"][..]],
        vec![&["value", "amount", "inf"][..]],
        vec![&["value", "unknown", "12"][..]],
        vec![&["value", "amount", "12"][..], &["value", "amount", "13"]],
        vec![
            &["value", "amount", "12"][..],
            &["value", "date", "2026-02-30"],
        ],
        vec![&["value", "amount", "12"][..], &["value", "ready", "yes"]],
        vec![&["value", "amount", "12"][..], &["value", "size", "large"]],
    ] {
        assert!(p.answer(&make(&values)).is_err(), "accepted {values:?}");
    }
}

#[test]
fn same_second_tie_break_is_independent_of_arrival_order() {
    let mut p = Prompt::parse(&prompt(&[])).unwrap();
    p.closes = "manual".into();
    let k = Keys::generate();
    let mut events = [response(&k, "approve", 1000), response(&k, "deny", 1000)];
    events.sort_by_key(|e| e.id);
    let mut state = InteractionState::default();
    for ev in events.iter().rev() {
        state
            .respond(&p, ev.clone(), p.answer(ev).unwrap(), 1000)
            .unwrap();
    }
    assert_eq!(
        state.votes[&k.public_key().to_hex()].source.id,
        events[0].id
    );
    assert!(state
        .respond(&p, events[1].clone(), p.answer(&events[1]).unwrap(), 1000)
        .is_err());
}

/// A hand-authored control trace: Discord-style multi-choice selection, then
/// GroupMe-style vote replacement before close. Expected counts are independent
/// of the implementation's tally calculation; see docs/interaction-controls.md.
#[test]
fn poll_control_trace_replaces_the_entire_selection_and_keeps_evidence() {
    let asker = Keys::generate();
    let prompt_event = event(
        KIND_INTERACTION_PROMPT,
        "Choose a thumbnail",
        &[
            &["itype", "poll"],
            &["opt", "a", "Door"],
            &["opt", "b", "Key"],
            &["opt", "c", "Hall"],
            &["min", "1"],
            &["max", "2"],
            &["closes", "manual"],
            &["deadline", "2000"],
        ],
        &asker,
        1000,
    );
    let p = Prompt::parse(&prompt_event).unwrap();
    let alice = Keys::generate();
    let bob = Keys::generate();
    let make = |key: &Keys, choices: &[&str], time| {
        let mut tags = vec![vec![
            "e".to_string(),
            prompt_event.id.to_hex(),
            "".into(),
            "prompt".into(),
        ]];
        tags.extend(choices.iter().map(|id| vec!["choice".into(), (*id).into()]));
        let tags: Vec<Vec<&str>> = tags
            .iter()
            .map(|t| t.iter().map(String::as_str).collect())
            .collect();
        event(
            KIND_INTERACTION_RESPONSE,
            "",
            &tags.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            key,
            time,
        )
    };
    for choices in [vec![], vec!["a", "b", "c"], vec!["a", "a"], vec!["unknown"]] {
        assert!(p.answer(&make(&alice, &choices, 1001)).is_err());
    }
    let first = make(&alice, &["a", "b"], 1001);
    let other = make(&bob, &["b"], 1001);
    let revised = make(&alice, &["c"], 1002);
    let mut state = InteractionState::default();
    for (ev, tally) in [
        (&first, serde_json::json!({"a":1,"b":1,"c":0})),
        (&other, serde_json::json!({"a":1,"b":2,"c":0})),
        (&revised, serde_json::json!({"a":0,"b":1,"c":1})),
    ] {
        ev.verify().unwrap();
        state
            .respond(&p, ev.clone(), p.answer(ev).unwrap(), 1002)
            .unwrap();
        let summary = state.summary(&p);
        assert_eq!(summary["tally"], tally);
        assert_eq!(summary["status"], "open");
        assert!(summary["winner"].is_null());
    }
    assert_eq!(state.votes.len(), 2);
    assert_eq!(
        state.votes[&alice.public_key().to_hex()].source.id,
        revised.id
    );
    state.close("manual");
    let late = make(&bob, &["a"], 1003);
    assert!(state
        .respond(&p, late.clone(), p.answer(&late).unwrap(), 1003)
        .is_err());
    assert_eq!(
        state.summary(&p)["tally"],
        serde_json::json!({"a":0,"b":1,"c":1})
    );
}

use crate::kind::{KIND_INTERACTION_CLOSE, KIND_STREAM_MESSAGE};

fn bare(kind: u32, content: &str, tags: &[&[&str]], keys: &Keys) -> Event {
    EventBuilder::new(Kind::Custom(kind as u16), content)
        .tags(tags.iter().map(|t| Tag::parse(t.iter().copied()).unwrap()))
        .custom_created_at(Timestamp::from(1000))
        .sign_with_keys(keys)
        .unwrap()
}

const CHANNEL: &str = "00000000-0000-0000-0000-000000000001";
const DL: &str = "2000";
const BUTTONS: &[&[&str]] = &[
    &["h", CHANNEL],
    &["itype", "buttons"],
    &["opt", "approve", "Approve"],
    &["closes", "first"],
    &["deadline", "2000"],
];

#[test]
fn envelope_limits_and_reserved_provenance_are_rejected() {
    let keys = Keys::generate();
    assert!(validate_envelope(&bare(KIND_INTERACTION_PROMPT, "ok", BUTTONS, &keys)).is_ok());
    let oversized = bare(KIND_INTERACTION_PROMPT, &"x".repeat(16_385), BUTTONS, &keys);
    assert!(validate_envelope(&oversized).is_err());
    let many: Vec<Vec<&str>> = (0..=MAX_TAGS).map(|_| vec!["t", "v"]).collect();
    let many: Vec<&[&str]> = many.iter().map(Vec::as_slice).collect();
    assert!(validate_envelope(&bare(KIND_INTERACTION_PROMPT, "ok", &many, &keys)).is_err());
    let big = "y".repeat(33_000);
    let heavy: &[&[&str]] = &[&["t", &big]];
    assert!(validate_envelope(&bare(KIND_INTERACTION_PROMPT, "ok", heavy, &keys)).is_err());
    for reserved in ["via", "actor", "interaction"] {
        let tags: &[&[&str]] = &[&["h", CHANNEL], &[reserved, "anything"]];
        assert!(
            validate_envelope(&bare(KIND_STREAM_MESSAGE, "ok", tags, &keys)).is_err(),
            "{reserved}"
        );
    }
    let expiring: &[&[&str]] = &[&["h", CHANNEL], &["expiration", "1"]];
    assert!(validate_envelope(&bare(KIND_STREAM_MESSAGE, "ok", expiring, &keys)).is_err());
}

#[test]
fn tag_helpers_demand_canonical_unique_values() {
    let keys = Keys::generate();
    let dup: &[&[&str]] = &[&["h", CHANNEL], &["itype", "poll"], &["itype", "buttons"]];
    let event = bare(KIND_INTERACTION_PROMPT, "q", dup, &keys);
    assert!(single_tag(&event, "itype").is_err());
    assert_eq!(single_tag(&event, "missing").unwrap(), None);
    let wide: &[&[&str]] = &[&["h", CHANNEL], &["closes", "first", "extra"]];
    assert!(single_tag(&bare(KIND_INTERACTION_PROMPT, "q", wide, &keys), "closes").is_err());
    let noncanonical: &[&[&str]] = &[&["h", "0F0F0F0F-0F0F-0F0F-0F0F-0F0F0F0F0F0F"]];
    assert!(channel(&bare(KIND_INTERACTION_PROMPT, "q", noncanonical, &keys)).is_err());
    let braces = format!("{{{CHANNEL}}}");
    let braced: &[&[&str]] = &[&["h", &braces]];
    assert!(channel(&bare(KIND_INTERACTION_PROMPT, "q", braced, &keys)).is_err());
    let missing: &[&[&str]] = &[&["itype", "buttons"]];
    assert!(channel(&bare(KIND_INTERACTION_PROMPT, "q", missing, &keys)).is_err());
    assert_eq!(
        channel(&bare(KIND_INTERACTION_PROMPT, "q", BUTTONS, &keys))
            .unwrap()
            .to_string(),
        CHANNEL
    );
    let id = "a".repeat(64);
    let root_only: &[&[&str]] = &[&["e", &id, "", "root"]];
    assert!(prompt_id(&bare(KIND_INTERACTION_RESPONSE, "", root_only, &keys)).is_err());
    let two: &[&[&str]] = &[&["e", &id, "", "prompt"], &["e", &id, "", "prompt"]];
    assert!(prompt_id(&bare(KIND_INTERACTION_RESPONSE, "", two, &keys)).is_err());
    let short: &[&[&str]] = &[&["e", &id, "prompt"]];
    assert!(prompt_id(&bare(KIND_INTERACTION_RESPONSE, "", short, &keys)).is_err());
    let bad_hex = "z".repeat(64);
    let invalid: &[&[&str]] = &[&["e", &bad_hex, "", "prompt"]];
    assert!(prompt_id(&bare(KIND_INTERACTION_RESPONSE, "", invalid, &keys)).is_err());
    let mixed: &[&[&str]] = &[&["e", &id, "", "root"], &["e", &id, "", "prompt"]];
    assert_eq!(
        prompt_id(&bare(KIND_INTERACTION_RESPONSE, "", mixed, &keys))
            .unwrap()
            .to_hex(),
        id
    );
}

#[test]
fn schema_limits_are_enforced_per_interaction_type() {
    let keys = Keys::generate();
    let parse = |tags: &[&[&str]]| Prompt::parse(&bare(KIND_INTERACTION_PROMPT, "q", tags, &keys));
    let mut nine: Vec<Vec<String>> = vec![
        vec!["h".into(), CHANNEL.into()],
        vec!["itype".into(), "buttons".into()],
        vec!["closes".into(), "first".into()],
        vec!["deadline".into(), "2000".into()],
    ];
    for i in 0..9 {
        nine.push(vec!["opt".into(), format!("o{i}"), format!("Option {i}")]);
    }
    fn owned(rows: &[Vec<String>]) -> Vec<Vec<&str>> {
        rows.iter()
            .map(|r| r.iter().map(String::as_str).collect())
            .collect()
    }
    let nine_ref = owned(&nine);
    let nine_ref: Vec<&[&str]> = nine_ref.iter().map(Vec::as_slice).collect();
    assert!(
        parse(&nine_ref).is_err(),
        "buttons accept at most 8 options"
    );
    let eight = owned(&nine[..12]);
    let eight: Vec<&[&str]> = eight.iter().map(Vec::as_slice).collect();
    assert!(parse(&eight).is_ok());

    let cases: &[(&str, &[&[&str]], bool)] = &[
        ("empty question is not a prompt", &[], false),
        (
            "unknown itype",
            &[
                &["h", CHANNEL],
                &["itype", "slider"],
                &["opt", "a", "A"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "missing closes",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "missing deadline",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["closes", "first"],
            ],
            false,
        ),
        (
            "non-numeric deadline",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["closes", "first"],
                &["deadline", "tomorrow"],
            ],
            false,
        ),
        (
            "quorum:0",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["closes", "quorum:0"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "quorum:257",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["closes", "quorum:257"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "quorum:1 is valid",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["closes", "quorum:1"],
                &["deadline", DL],
            ],
            true,
        ),
        (
            "quorum:256 is valid",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["closes", "quorum:256"],
                &["deadline", DL],
            ],
            true,
        ),
        (
            "poll cannot close first",
            &[
                &["h", CHANNEL],
                &["itype", "poll"],
                &["opt", "a", "A"],
                &["opt", "b", "B"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "poll needs two options",
            &[
                &["h", CHANNEL],
                &["itype", "poll"],
                &["opt", "a", "A"],
                &["closes", "expiry"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "poll cannot carry fields",
            &[
                &["h", CHANNEL],
                &["itype", "poll"],
                &["opt", "a", "A"],
                &["opt", "b", "B"],
                &["field", "n", "N", "text", "optional"],
                &["closes", "expiry"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "poll min zero",
            &[
                &["h", CHANNEL],
                &["itype", "poll"],
                &["opt", "a", "A"],
                &["opt", "b", "B"],
                &["min", "0"],
                &["closes", "expiry"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "poll max above options",
            &[
                &["h", CHANNEL],
                &["itype", "poll"],
                &["opt", "a", "A"],
                &["opt", "b", "B"],
                &["max", "3"],
                &["closes", "expiry"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "poll min above max",
            &[
                &["h", CHANNEL],
                &["itype", "poll"],
                &["opt", "a", "A"],
                &["opt", "b", "B"],
                &["min", "2"],
                &["max", "1"],
                &["closes", "expiry"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "poll multi-select",
            &[
                &["h", CHANNEL],
                &["itype", "poll"],
                &["opt", "a", "A"],
                &["opt", "b", "B"],
                &["min", "1"],
                &["max", "2"],
                &["closes", "manual"],
                &["deadline", DL],
            ],
            true,
        ),
        (
            "form needs a field",
            &[
                &["h", CHANNEL],
                &["itype", "form"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "form cannot carry options",
            &[
                &["h", CHANNEL],
                &["itype", "form"],
                &["opt", "a", "A"],
                &["field", "n", "N", "text", "optional"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "select field needs options",
            &[
                &["h", CHANNEL],
                &["itype", "form"],
                &["field", "s", "S", "select", "required"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "optsel must reference a select field",
            &[
                &["h", CHANNEL],
                &["itype", "form"],
                &["field", "n", "N", "text", "optional"],
                &["optsel", "n", "a"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "duplicate optsel value",
            &[
                &["h", CHANNEL],
                &["itype", "form"],
                &["field", "s", "S", "select", "required"],
                &["optsel", "s", "a"],
                &["optsel", "s", "a"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "field requirement must be required or optional",
            &[
                &["h", CHANNEL],
                &["itype", "form"],
                &["field", "n", "N", "text", "maybe"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "duplicate field id",
            &[
                &["h", CHANNEL],
                &["itype", "form"],
                &["field", "n", "N", "text", "optional"],
                &["field", "n", "M", "text", "optional"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "option id with spaces",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "not ok", "A"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "option label with control characters",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A\u{7}"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "unknown option style",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A", "warning"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "buttons demand exactly one choice",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["opt", "b", "B"],
                &["min", "2"],
                &["max", "2"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "unsupported responder rule",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["responders", "role:member"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "invalid listed public key",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["responders", "listed"],
                &["p", "nope"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
        (
            "explicit public visibility",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["visibility", "public"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            true,
        ),
        (
            "wrong kind",
            &[
                &["h", CHANNEL],
                &["itype", "buttons"],
                &["opt", "a", "A"],
                &["closes", "first"],
                &["deadline", DL],
            ],
            false,
        ),
    ];
    for &(name, tags, ok) in cases {
        let event = if name == "empty question is not a prompt" {
            bare(KIND_INTERACTION_PROMPT, "  \n", BUTTONS, &keys)
        } else if name == "wrong kind" {
            bare(KIND_INTERACTION_RESPONSE, "q", tags, &keys)
        } else {
            bare(KIND_INTERACTION_PROMPT, "q", tags, &keys)
        };
        assert_eq!(Prompt::parse(&event).is_ok(), ok, "{name}");
    }

    let many_keys: Vec<String> = (0..257)
        .map(|_| Keys::generate().public_key().to_hex())
        .collect();
    let mut listed: Vec<Vec<&str>> = vec![
        vec!["h", CHANNEL],
        vec!["itype", "buttons"],
        vec!["opt", "a", "A"],
        vec!["responders", "listed"],
        vec!["closes", "first"],
        vec!["deadline", DL],
    ];
    listed.extend(many_keys.iter().map(|k| vec!["p", k.as_str()]));
    let too_many: Vec<&[&str]> = listed.iter().map(Vec::as_slice).collect();
    assert!(parse(&too_many).is_err(), "at most 256 listed responders");
    listed.pop();
    let max: Vec<&[&str]> = listed.iter().map(Vec::as_slice).collect();
    let p = parse(&max).unwrap();
    assert_eq!(p.listed.len(), 256);
    assert_eq!(p.responders, "listed");
}

#[test]
fn text_answers_match_labels_or_ids_only_for_simple_prompts() {
    let keys = Keys::generate();
    let tags: &[&[&str]] = &[
        &["h", CHANNEL],
        &["itype", "buttons"],
        &["opt", "approve", "Approve", "primary"],
        &["opt", "revise", "Send back"],
        &["field", "note", "Note", "text", "optional"],
        &["closes", "first"],
        &["deadline", "2000"],
    ];
    let p = Prompt::parse(&bare(KIND_INTERACTION_PROMPT, "q", tags, &keys)).unwrap();
    assert_eq!(
        p.text_answer("send BACK").unwrap().choices,
        BTreeSet::from(["revise".to_string()])
    );
    assert_eq!(
        p.text_answer("Approve").unwrap().choices,
        BTreeSet::from(["approve".to_string()])
    );
    assert!(p.text_answer("send back!").is_none());
    assert!(p.text_answer("approve revise").is_none());
    let required: &[&[&str]] = &[
        &["h", CHANNEL],
        &["itype", "buttons"],
        &["opt", "approve", "Approve"],
        &["field", "note", "Note", "text", "required"],
        &["closes", "first"],
        &["deadline", "2000"],
    ];
    let p = Prompt::parse(&bare(KIND_INTERACTION_PROMPT, "q", required, &keys)).unwrap();
    assert!(
        p.text_answer("approve").is_none(),
        "required fields cannot be typed"
    );
    let multi: &[&[&str]] = &[
        &["h", CHANNEL],
        &["itype", "poll"],
        &["opt", "a", "A"],
        &["opt", "b", "B"],
        &["min", "2"],
        &["max", "2"],
        &["closes", "manual"],
        &["deadline", "2000"],
    ];
    let p = Prompt::parse(&bare(KIND_INTERACTION_PROMPT, "q", multi, &keys)).unwrap();
    assert!(
        p.text_answer("a").is_none(),
        "a single word cannot satisfy min 2"
    );
}

#[test]
fn answers_validate_channel_sizes_and_field_formats() {
    let keys = Keys::generate();
    let tags: &[&[&str]] = &[
        &["h", CHANNEL],
        &["itype", "form"],
        &["field", "amount", "Amount", "number", "optional"],
        &["field", "note", "Note", "text", "optional"],
        &["field", "when", "When", "date", "optional"],
        &["field", "flag", "Flag", "boolean", "optional"],
        &["closes", "first"],
        &["deadline", "2000"],
    ];
    let prompt = bare(KIND_INTERACTION_PROMPT, "q", tags, &keys);
    let p = Prompt::parse(&prompt).unwrap();
    let id = prompt.id.to_hex();
    let answer = |channel: &str, values: &[&[&str]]| {
        let mut ts: Vec<Vec<&str>> = vec![vec!["h", channel], vec!["e", &id, "", "prompt"]];
        ts.extend(values.iter().map(|v| v.to_vec()));
        let ts: Vec<&[&str]> = ts.iter().map(Vec::as_slice).collect();
        bare(KIND_INTERACTION_RESPONSE, "", &ts, &keys)
    };
    assert!(
        p.answer(&answer(CHANNEL, &[])).is_ok(),
        "no required fields"
    );
    assert!(p
        .answer(&answer("00000000-0000-0000-0000-000000000002", &[]))
        .is_err());
    assert!(p
        .answer(&answer(CHANNEL, &[&["value", "amount", "1e3"]]))
        .is_ok());
    assert!(p
        .answer(&answer(CHANNEL, &[&["value", "amount", ""]]))
        .is_err());
    assert!(p
        .answer(&answer(CHANNEL, &[&["value", "note", ""]]))
        .is_ok());
    assert!(p
        .answer(&answer(CHANNEL, &[&["value", "when", "2026-9-8"]]))
        .is_err());
    assert!(p
        .answer(&answer(CHANNEL, &[&["value", "flag", "True"]]))
        .is_err());
    assert!(p.answer(&answer(CHANNEL, &[&["value", "note"]])).is_err());
    assert!(p
        .answer(&answer(CHANNEL, &[&["choice", "anything"]]))
        .is_err());
    let long = "n".repeat(4097);
    assert!(p
        .answer(&answer(CHANNEL, &[&["value", "note", &long]]))
        .is_err());
    let chunk = "n".repeat(4096);
    let three: &[&[&str]] = &[
        &["value", "note", &chunk],
        &["value", "when", "2026-09-08"],
        &["value", "amount", &chunk],
    ];
    assert!(
        p.answer(&answer(CHANNEL, three)).is_err(),
        "values exceed 8 KiB"
    );
    let wrong_kind = bare(
        KIND_INTERACTION_CLOSE,
        "",
        &[&["h", CHANNEL], &["e", &id, "", "prompt"]],
        &keys,
    );
    assert!(p.answer(&wrong_kind).is_err());
}

#[test]
fn responder_cap_replacement_and_deadline_bound_the_state() {
    let keys = Keys::generate();
    let tags: &[&[&str]] = &[
        &["h", CHANNEL],
        &["itype", "buttons"],
        &["opt", "a", "A"],
        &["closes", "manual"],
        &["deadline", "2000"],
    ];
    let prompt = bare(KIND_INTERACTION_PROMPT, "q", tags, &keys);
    let p = Prompt::parse(&prompt).unwrap();
    let id = prompt.id.to_hex();
    let vote = |k: &Keys, time: u64| {
        let ts: &[&[&str]] = &[&["h", CHANNEL], &["e", &id, "", "prompt"], &["choice", "a"]];
        EventBuilder::new(Kind::Custom(KIND_INTERACTION_RESPONSE as u16), "")
            .tags(ts.iter().map(|t| Tag::parse(t.iter().copied()).unwrap()))
            .custom_created_at(Timestamp::from(time))
            .sign_with_keys(k)
            .unwrap()
    };
    let mut state = InteractionState::default();
    let first = Keys::generate();
    let mut responders = vec![first.clone()];
    responders.extend((1..MAX_RESPONDERS).map(|_| Keys::generate()));
    for k in &responders {
        let ev = vote(k, 1000);
        assert!(state
            .respond(&p, ev.clone(), p.answer(&ev).unwrap(), 1000)
            .unwrap());
    }
    assert_eq!(state.votes.len(), MAX_RESPONDERS);
    let extra = vote(&Keys::generate(), 1001);
    assert!(state
        .respond(&p, extra.clone(), p.answer(&extra).unwrap(), 1001)
        .is_err());
    // An existing responder may still replace their own answer at the cap.
    let replaced = vote(&first, 1001);
    assert!(state
        .respond(&p, replaced.clone(), p.answer(&replaced).unwrap(), 1001)
        .unwrap());
    assert_eq!(state.revision, MAX_RESPONDERS as u64 + 1);
    // The deadline is enforced on every transition, before any sweep runs.
    let late = vote(&Keys::generate(), 1999);
    assert!(state
        .respond(&p, late.clone(), p.answer(&late).unwrap(), 2000)
        .is_err());
    let summary = state.summary(&p);
    assert_eq!(
        summary["responders"].as_array().unwrap().len(),
        MAX_RESPONDERS
    );
    assert!(
        summary["values"].is_null(),
        "values is a one-responder shortcut"
    );
    assert_eq!(summary["status"], "open");
    assert!(summary["winner"].is_null(), "no winner while open");
    assert!(state.close("manual"));
    assert_eq!(state.summary(&p)["winner"], "a");
}

#[test]
fn state_events_are_addressable_by_prompt_and_carry_a_single_responder_shortcut() {
    let keys = Keys::generate();
    let tags: &[&[&str]] = &[
        &["h", CHANNEL],
        &["itype", "form"],
        &["field", "title", "Title", "text", "required"],
        &["closes", "first"],
        &["deadline", "2000"],
    ];
    let prompt = bare(KIND_INTERACTION_PROMPT, "q", tags, &keys);
    let p = Prompt::parse(&prompt).unwrap();
    let id = prompt.id.to_hex();
    let ts: &[&[&str]] = &[
        &["h", CHANNEL],
        &["e", &id, "", "prompt"],
        &["value", "title", "The Door"],
    ];
    let response = bare(KIND_INTERACTION_RESPONSE, "", ts, &keys);
    let mut state = InteractionState::default();
    state
        .respond(&p, response.clone(), p.answer(&response).unwrap(), 1000)
        .unwrap();
    let summary = state.summary(&p);
    assert_eq!(summary["values"]["title"], "The Door");
    assert_eq!(summary["close_reason"], "first");
    assert!(summary["winner"].is_null(), "forms never elect a winner");
    assert_eq!(summary["responders"][0]["event_id"], response.id.to_hex());
    let relay = Keys::generate();
    let event = state
        .event_builder(prompt.id, &p, 4242)
        .unwrap()
        .sign_with_keys(&relay)
        .unwrap();
    assert_eq!(event.kind.as_u16() as u32, KIND_INTERACTION_STATE);
    assert_eq!(event.created_at.as_secs(), 4242);
    assert_eq!(single_tag(&event, "d").unwrap(), Some(id.as_str()));
    assert_eq!(channel(&event).unwrap().to_string(), CHANNEL);
    let content: serde_json::Value = serde_json::from_str(&event.content).unwrap();
    assert_eq!(content["revision"], 1);
    assert_eq!(content["version"], 1);
}
