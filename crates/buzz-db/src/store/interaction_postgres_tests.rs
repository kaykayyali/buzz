use super::*;
use buzz_core::kind::KIND_INTERACTION_STATE;
use nostr::Timestamp;

struct Fixture {
    db: Db,
    community: CommunityId,
    channel: Uuid,
    asker: Keys,
    alice: Keys,
    bob: Keys,
    relay: Keys,
}
impl Fixture {
    async fn new() -> Self {
        let pool = sqlx::PgPool::connect(&crate::test_support::database_url())
            .await
            .unwrap();
        let db = Db::from_pool(pool.clone());
        // The outbox and expiry batches are deployment-wide. Earlier runs and
        // sibling tests never acknowledge their rows, so a shared database
        // accumulates a backlog that would otherwise crowd out this fixture.
        sqlx::query(
            "DELETE FROM interaction_outbox WHERE queued_at < now() - interval '5 minutes'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let community = CommunityId::from_uuid(Uuid::new_v4());
        let channel = Uuid::new_v4();
        let asker = Keys::generate();
        let alice = Keys::generate();
        let bob = Keys::generate();
        sqlx::query("INSERT INTO communities(id,host) VALUES($1,$2)")
            .bind(community.as_uuid())
            .bind(format!("interactions-{}.test", Uuid::new_v4()))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO channels(id,community_id,name,created_by) VALUES($1,$2,'interactions',$3)",
        )
        .bind(channel)
        .bind(community.as_uuid())
        .bind(asker.public_key().to_bytes().as_slice())
        .execute(&pool)
        .await
        .unwrap();
        for key in [&asker, &alice, &bob] {
            sqlx::query(
                "INSERT INTO channel_members(community_id,channel_id,pubkey) VALUES($1,$2,$3)",
            )
            .bind(community.as_uuid())
            .bind(channel)
            .bind(key.public_key().to_bytes().as_slice())
            .execute(&pool)
            .await
            .unwrap();
        }
        Self {
            db,
            community,
            channel,
            asker,
            alice,
            bob,
            relay: Keys::generate(),
        }
    }
    fn prompt(&self, closes: &str, responders: &str) -> Event {
        self.event(
            &self.asker,
            KIND_INTERACTION_PROMPT,
            "Choose",
            vec![
                vec!["itype", "buttons"],
                vec!["opt", "yes", "Yes"],
                vec!["opt", "no", "No"],
                vec!["closes", closes],
                vec!["responders", responders],
                vec!["deadline", &(Timestamp::now().as_secs() + 3600).to_string()],
            ],
        )
    }
    fn event(&self, keys: &Keys, kind: u32, content: &str, ts: Vec<Vec<&str>>) -> Event {
        let mut tags = vec![Tag::parse(["h", &self.channel.to_string()]).unwrap()];
        tags.extend(ts.into_iter().map(|t| Tag::parse(t).unwrap()));
        EventBuilder::new(Kind::Custom(kind as u16), content)
            .tags(tags)
            .sign_with_keys(keys)
            .unwrap()
    }
    fn answer(&self, key: &Keys, p: &Event, choice: &str) -> Event {
        self.event(
            key,
            KIND_INTERACTION_RESPONSE,
            "",
            vec![
                vec!["e", &p.id.to_hex(), "", "prompt"],
                vec!["choice", choice],
            ],
        )
    }
    fn close(&self, key: &Keys, p: &Event) -> Event {
        self.event(
            key,
            KIND_INTERACTION_CLOSE,
            "",
            vec![vec!["e", &p.id.to_hex(), "", "prompt"]],
        )
    }
    async fn add_member(&self, key: &Keys) {
        sqlx::query("INSERT INTO channel_members(community_id,channel_id,pubkey) VALUES($1,$2,$3)")
            .bind(self.community.as_uuid())
            .bind(self.channel)
            .bind(key.public_key().to_bytes().as_slice())
            .execute(&self.db.pool)
            .await
            .unwrap();
    }
    async fn closed(&self, p: &Event) -> bool {
        sqlx::query_scalar("SELECT closed FROM interactions WHERE community_id=$1 AND prompt_id=$2")
            .bind(self.community.as_uuid())
            .bind(p.id.as_bytes().as_slice())
            .fetch_one(&self.db.pool)
            .await
            .unwrap()
    }
    /// Claim this fixture's queued rows through the production claim query.
    async fn claimed(&self) -> (InteractionDeliveryBatch, Vec<InteractionDelivery>) {
        let batch = self
            .db
            .claim_interaction_events_scoped(Some(self.community))
            .await
            .unwrap();
        let ours = batch.deliveries.clone();
        (batch, ours)
    }
    /// Run the deployment-wide expiry sweep until `p` is closed. A sibling
    /// fixture's sweep may close it first, so the returned deliveries are only
    /// those this call observed for this fixture's community.
    async fn sweep_until_closed(&self, p: &Event) -> Vec<InteractionDelivery> {
        let mut ours = Vec::new();
        for _ in 0..5 {
            ours.extend(
                self.db
                    .expire_interactions(&self.relay)
                    .await
                    .unwrap()
                    .into_iter()
                    .filter(|d| d.community == self.community),
            );
            if self.closed(p).await {
                return ours;
            }
        }
        panic!("prompt was never closed by the sweep");
    }
    async fn accept(&self, e: &Event) -> Result<Option<Vec<StoredEvent>>> {
        self.db
            .accept_interaction(self.community, e, &self.relay, None)
            .await
    }
    async fn state(&self, p: &Event) -> InteractionState {
        let value: serde_json::Value = sqlx::query_scalar(
            "SELECT state FROM interactions WHERE community_id=$1 AND prompt_id=$2",
        )
        .bind(self.community.as_uuid())
        .bind(p.id.as_bytes().as_slice())
        .fetch_one(&self.db.pool)
        .await
        .unwrap();
        serde_json::from_value(value).unwrap()
    }
    async fn stored(&self, e: &Event) -> bool {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM events WHERE community_id=$1 AND id=$2)")
            .bind(self.community.as_uuid())
            .bind(e.id.as_bytes().as_slice())
            .fetch_one(&self.db.pool)
            .await
            .unwrap()
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn concurrent_first_answers_commit_exactly_one_decision() {
    let f = Fixture::new().await;
    let p = f.prompt("first", "members");
    let emitted = f.accept(&p).await.unwrap().unwrap();
    assert_eq!(emitted.len(), 3);
    assert_eq!(
        emitted
            .iter()
            .filter(|e| e.event.kind.as_u16() as u32 == KIND_STREAM_MESSAGE)
            .count(),
        1
    );
    let a = f.answer(&f.alice, &p, "yes");
    let b = f.answer(&f.bob, &p, "no");
    let (a_result, b_result) = tokio::join!(f.accept(&a), f.accept(&b));
    assert_ne!(a_result.is_ok(), b_result.is_ok());
    assert_ne!(f.stored(&a).await, f.stored(&b).await);
    let state = f.state(&p).await;
    assert_eq!(state.votes.len(), 1);
    assert_eq!(state.close_reason.as_deref(), Some("first"));
    let winner = if a_result.is_ok() { &a } else { &b };
    assert!(f.accept(winner).await.unwrap().unwrap().is_empty());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE community_id=$1 AND kind=$2 AND deleted_at IS NULL",
    )
    .bind(f.community.as_uuid())
    .bind(KIND_INTERACTION_STATE as i32)
    .fetch_one(&f.db.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn rejected_answers_are_not_stored_and_roles_are_live() {
    let f = Fixture::new().await;
    let p = f.prompt("manual", "role:owner");
    f.accept(&p).await.unwrap();
    let a = f.answer(&f.alice, &p, "yes");
    assert!(f.accept(&a).await.is_err());
    assert!(!f.stored(&a).await);
    sqlx::query("INSERT INTO relay_members(community_id,pubkey,role) VALUES($1,$2,'owner')")
        .bind(f.community.as_uuid())
        .bind(f.alice.public_key().to_hex())
        .execute(&f.db.pool)
        .await
        .unwrap();
    f.accept(&a).await.unwrap();
    let stranger = f.answer(&Keys::generate(), &p, "yes");
    assert!(f.accept(&stranger).await.is_err());
    let invalid = f.answer(&f.alice, &p, "maybe");
    assert!(f.accept(&invalid).await.is_err());
    assert!(!f.stored(&invalid).await);
    let other = Fixture::new().await;
    assert!(other
        .db
        .accept_interaction(other.community, &a, &other.relay, None)
        .await
        .is_err());
    let close = f.event(
        &f.bob,
        KIND_INTERACTION_CLOSE,
        "",
        vec![vec!["e", &p.id.to_hex(), "", "prompt"]],
    );
    assert!(f.accept(&close).await.is_err());
    sqlx::query("UPDATE channel_members SET removed_at=now() WHERE community_id=$1 AND channel_id=$2 AND pubkey=$3").bind(f.community.as_uuid()).bind(f.channel).bind(f.alice.public_key().to_bytes().as_slice()).execute(&f.db.pool).await.unwrap();
    let changed = f.answer(&f.alice, &p, "no");
    assert!(f.accept(&changed).await.is_err());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn text_fallback_preserves_signed_source_and_actor_deduplication() {
    let f = Fixture::new().await;
    let p = f.prompt("manual", "members");
    let rows = f.accept(&p).await.unwrap().unwrap();
    let projection = rows
        .iter()
        .find(|e| e.event.kind.as_u16() as u32 == KIND_STREAM_MESSAGE)
        .unwrap();
    let reply = f.event(
        &f.alice,
        KIND_STREAM_MESSAGE,
        " YES \n",
        vec![vec!["e", &projection.event.id.to_hex(), "", "reply"]],
    );
    let accepted = f.accept(&reply).await.unwrap().unwrap();
    let synthetic = accepted
        .iter()
        .find(|e| e.event.kind.as_u16() as u32 == KIND_INTERACTION_RESPONSE)
        .unwrap();
    assert_eq!(synthetic.event.pubkey, f.relay.public_key());
    synthetic.event.verify().unwrap();
    assert_eq!(
        interaction::single_tag(&synthetic.event, "via").unwrap(),
        Some(reply.id.to_hex().as_str())
    );
    assert_eq!(
        f.state(&p).await.votes[&f.alice.public_key().to_hex()].source,
        reply
    );
    let prose = f.event(
        &f.bob,
        KIND_STREAM_MESSAGE,
        "yes but revise",
        vec![vec!["e", &projection.event.id.to_hex(), "", "reply"]],
    );
    assert!(f.accept(&prose).await.unwrap().is_none());
    let later = EventBuilder::new(Kind::Custom(KIND_INTERACTION_RESPONSE as u16), "")
        .tags(f.answer(&f.alice, &p, "no").tags.to_vec())
        .custom_created_at(Timestamp::from(reply.created_at.as_secs() + 1))
        .sign_with_keys(&f.alice)
        .unwrap();
    f.accept(&later).await.unwrap();
    assert_eq!(f.state(&p).await.votes.len(), 1);
    assert_eq!(
        f.state(&p).await.summary(&Prompt::parse(&p).unwrap())["tally"]["yes"],
        0
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn state_failure_rolls_back_response_and_tally() {
    let f = Fixture::new().await;
    let p = f.prompt("manual", "members");
    f.accept(&p).await.unwrap();
    // A newer authoritative coordinate forces the production replacement seam
    // to reject our next state. The answer INSERT must roll back with it.
    let schema = Prompt::parse(&p).unwrap();
    let future = InteractionState::default()
        .event_builder(p.id, &schema, Timestamp::now().as_secs() + 100)
        .unwrap()
        .sign_with_keys(&f.relay)
        .unwrap();
    f.db.replace_parameterized_event(f.community, &future, &p.id.to_hex(), Some(f.channel))
        .await
        .unwrap();
    let answer = f.answer(&f.alice, &p, "yes");
    assert!(f.accept(&answer).await.is_err());
    assert!(!f.stored(&answer).await);
    assert!(f.state(&p).await.votes.is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn expiry_is_durable_and_rejects_answers_before_sweep() {
    let f = Fixture::new().await;
    let base = f.prompt("manual", "members");
    let mut tags = base.tags.clone().to_vec();
    tags.retain(|t| t.kind().to_string() != "deadline");
    tags.push(Tag::parse(["deadline", &(Timestamp::now().as_secs() + 2).to_string()]).unwrap());
    let p = EventBuilder::new(base.kind, base.content)
        .tags(tags)
        .sign_with_keys(&f.asker)
        .unwrap();
    f.accept(&p).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let answer = f.answer(&f.alice, &p, "yes");
    assert!(f.accept(&answer).await.is_err());
    assert!(!f.stored(&answer).await);
    let ours = f.sweep_until_closed(&p).await;
    assert!(ours.len() <= 1);
    for delivery in &ours {
        assert_eq!(delivery.channel, f.channel);
        assert_eq!(delivery.event.pubkey, f.relay.public_key());
        assert_eq!(
            delivery.event.kind.as_u16() as u32,
            KIND_INTERACTION_STATE,
            "the sweep returns the committed state event for auditing"
        );
        assert_eq!(
            interaction::single_tag(&delivery.event, "d").unwrap(),
            Some(p.id.to_hex().as_str())
        );
    }
    assert_eq!(f.state(&p).await.close_reason.as_deref(), Some("expiry"));
    assert!(f
        .db
        .expire_interactions(&f.relay)
        .await
        .unwrap()
        .iter()
        .all(|d| d.community != f.community));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn expiry_sweep_skips_a_poison_row_and_still_closes_the_rest() {
    let f = Fixture::new().await;
    let make = |content: &str| {
        let base = f.prompt("manual", "members");
        let mut tags = base.tags.clone().to_vec();
        tags.retain(|t| t.kind().to_string() != "deadline");
        tags.push(Tag::parse(["deadline", &(Timestamp::now().as_secs() + 2).to_string()]).unwrap());
        EventBuilder::new(base.kind, content)
            .tags(tags)
            .sign_with_keys(&f.asker)
            .unwrap()
    };
    let poison = make("poison");
    let healthy = make("healthy");
    f.accept(&poison).await.unwrap();
    f.accept(&healthy).await.unwrap();
    // A stored schema that no longer parses must not block the whole sweep.
    sqlx::query(
        "UPDATE interactions SET prompt='{}'::jsonb WHERE community_id=$1 AND prompt_id=$2",
    )
    .bind(f.community.as_uuid())
    .bind(poison.id.as_bytes().as_slice())
    .execute(&f.db.pool)
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let ours = f.sweep_until_closed(&healthy).await;
    assert!(ours.iter().all(|d| {
        interaction::single_tag(&d.event, "d").unwrap() == Some(healthy.id.to_hex().as_str())
    }));
    assert!(f.closed(&healthy).await);
    assert_eq!(
        f.state(&healthy).await.close_reason.as_deref(),
        Some("expiry")
    );
    // The poison row is quarantined: marked closed with no state transition, so
    // it leaves the bounded sweep window instead of pinning it forever.
    assert!(f.closed(&poison).await);
    let raw: serde_json::Value =
        sqlx::query_scalar("SELECT state FROM interactions WHERE community_id=$1 AND prompt_id=$2")
            .bind(f.community.as_uuid())
            .bind(poison.id.as_bytes().as_slice())
            .fetch_one(&f.db.pool)
            .await
            .unwrap();
    assert!(raw["close_reason"].is_null());
    assert!(f
        .db
        .expire_interactions(&f.relay)
        .await
        .unwrap()
        .iter()
        .all(|d| d.community != f.community));
    // An operator who repairs the row can reopen it for the next sweep.
    sqlx::query(
        "UPDATE interactions SET prompt=$3, closed=FALSE WHERE community_id=$1 AND prompt_id=$2",
    )
    .bind(f.community.as_uuid())
    .bind(poison.id.as_bytes().as_slice())
    .bind(serde_json::to_value(&poison).unwrap())
    .execute(&f.db.pool)
    .await
    .unwrap();
    let repaired = f.sweep_until_closed(&poison).await;
    assert!(repaired.len() <= 1);
    assert_eq!(
        f.state(&poison).await.close_reason.as_deref(),
        Some("expiry")
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn claimed_outbox_rows_retry_until_committed_and_hide_from_other_workers() {
    let f = Fixture::new().await;
    let p = f.prompt("first", "members");
    let initial = f.accept(&p).await.unwrap().unwrap();
    let initial_state = initial
        .iter()
        .find(|e| e.event.kind.as_u16() as u32 == KIND_INTERACTION_STATE)
        .unwrap();
    let answer = f.answer(&f.alice, &p, "yes");
    f.accept(&answer).await.unwrap();
    f.db.soft_delete_event(f.community, p.id.as_bytes())
        .await
        .unwrap();
    let (first, ours) = f.claimed().await;
    assert!(ours.iter().any(|e| e.event.id == answer.id));
    assert!(!ours
        .iter()
        .any(|e| e.event.id == p.id || e.event.id == initial_state.event.id));
    // A second worker must not publish rows the first one is delivering.
    let (second, others) = f.claimed().await;
    assert!(
        others.is_empty(),
        "claimed rows are invisible to a concurrent claim"
    );
    second.commit().await.unwrap();
    // Rolling back without a commit retries the claim with nothing acknowledged.
    first.release().await.unwrap();
    let (mut batch, ours) = f.claimed().await;
    assert!(ours.iter().any(|e| e.event.id == answer.id));
    batch.acknowledge(f.community, &answer).await.unwrap();
    // An acknowledgement is not durable until the batch commits.
    let (peek, still_pending) = f.claimed().await;
    assert!(
        still_pending.is_empty(),
        "the row is still claimed by the open batch"
    );
    peek.commit().await.unwrap();
    batch.commit().await.unwrap();
    let (batch, ours) = f.claimed().await;
    batch.commit().await.unwrap();
    assert!(!ours.iter().any(|e| e.event.id == answer.id));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn asker_close_is_authoritative_and_exact_replays_stay_idempotent() {
    let f = Fixture::new().await;
    let p = f.prompt("manual", "members");
    f.accept(&p).await.unwrap();
    let first = f.answer(&f.alice, &p, "yes");
    f.accept(&first).await.unwrap();
    let close = f.close(&f.asker, &p);
    let emitted = f.accept(&close).await.unwrap().unwrap();
    assert!(emitted.iter().any(|e| e.event.id == close.id));
    assert!(f.closed(&p).await);
    let state = f.state(&p).await;
    assert_eq!(state.close_reason.as_deref(), Some("manual"));
    assert_eq!(state.revision, 2);
    // A new answer after close is rejected and never stored.
    let late = f.answer(&f.bob, &p, "no");
    assert!(f.accept(&late).await.is_err());
    assert!(!f.stored(&late).await);
    // The exact earlier answer replays as a harmless duplicate.
    assert!(f.accept(&first).await.unwrap().unwrap().is_empty());
    // An exact replay of the close is idempotent; a distinct second close is rejected.
    assert!(f.accept(&close).await.unwrap().unwrap().is_empty());
    let again = EventBuilder::new(close.kind, "")
        .tags(close.tags.to_vec())
        .custom_created_at(Timestamp::from(close.created_at.as_secs() + 1))
        .sign_with_keys(&f.asker)
        .unwrap();
    assert!(matches!(
        f.accept(&again).await,
        Err(DbError::InvalidData(_))
    ));
    assert!(!f.stored(&again).await);
    let tagged = f.event(
        &f.asker,
        KIND_INTERACTION_CLOSE,
        "",
        vec![
            vec!["e", &p.id.to_hex(), "", "prompt"],
            vec!["choice", "yes"],
        ],
    );
    assert!(matches!(
        f.accept(&tagged).await,
        Err(DbError::InvalidData(_))
    ));
    assert_eq!(f.state(&p).await.revision, 2);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn quorum_counts_distinct_actors_and_rejects_projection_references() {
    let f = Fixture::new().await;
    let p = f.prompt("quorum:2", "members");
    let rows = f.accept(&p).await.unwrap().unwrap();
    let projection = rows
        .iter()
        .find(|e| e.event.kind.as_u16() as u32 == KIND_STREAM_MESSAGE)
        .unwrap();
    // A typed response must name the signed prompt, not its text projection.
    let via_projection = f.answer(&f.alice, &projection.event, "yes");
    assert!(matches!(
        f.accept(&via_projection).await,
        Err(DbError::InvalidData(_))
    ));
    let a1 = f.answer(&f.alice, &p, "yes");
    f.accept(&a1).await.unwrap();
    let a2 = EventBuilder::new(Kind::Custom(KIND_INTERACTION_RESPONSE as u16), "")
        .tags(f.answer(&f.alice, &p, "no").tags.to_vec())
        .custom_created_at(Timestamp::from(a1.created_at.as_secs() + 1))
        .sign_with_keys(&f.alice)
        .unwrap();
    f.accept(&a2).await.unwrap();
    assert!(
        !f.closed(&p).await,
        "one actor changing their mind is not a quorum"
    );
    let b = f.answer(&f.bob, &p, "yes");
    f.accept(&b).await.unwrap();
    let state = f.state(&p).await;
    assert_eq!(state.close_reason.as_deref(), Some("quorum"));
    assert_eq!(state.votes.len(), 2);
    let summary = state.summary(&Prompt::parse(&p).unwrap());
    assert_eq!(summary["tally"]["yes"], 1);
    assert_eq!(summary["tally"]["no"], 1);
    assert!(summary["winner"].is_null());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn listed_responders_prompt_replies_and_relay_replies_are_filtered() {
    let f = Fixture::new().await;
    let alice_hex = f.alice.public_key().to_hex();
    let p = f.event(
        &f.asker,
        KIND_INTERACTION_PROMPT,
        "Choose",
        vec![
            vec!["itype", "buttons"],
            vec!["opt", "yes", "Yes"],
            vec!["opt", "no", "No"],
            vec!["closes", "manual"],
            vec!["responders", "listed"],
            vec!["p", &alice_hex],
            vec!["deadline", &(Timestamp::now().as_secs() + 3600).to_string()],
        ],
    );
    f.accept(&p).await.unwrap();
    // Bob may read but not answer: typed answers fail, text stays a comment.
    assert!(matches!(
        f.accept(&f.answer(&f.bob, &p, "yes")).await,
        Err(DbError::AccessDenied(_))
    ));
    let bob_text = f.event(
        &f.bob,
        KIND_STREAM_MESSAGE,
        "yes",
        vec![vec!["e", &p.id.to_hex(), "", "reply"]],
    );
    assert!(f.accept(&bob_text).await.unwrap().is_none());
    // The relay's own text never becomes a vote.
    let relay_text = f.event(
        &f.relay,
        KIND_STREAM_MESSAGE,
        "yes",
        vec![vec!["e", &p.id.to_hex(), "", "reply"]],
    );
    assert!(f.accept(&relay_text).await.unwrap().is_none());
    // A listed responder may answer by replying to the signed prompt itself.
    let alice_text = f.event(
        &f.alice,
        KIND_STREAM_MESSAGE,
        "No",
        vec![vec!["e", &p.id.to_hex(), "", "reply"]],
    );
    let accepted = f.accept(&alice_text).await.unwrap().unwrap();
    assert!(accepted
        .iter()
        .any(|e| e.event.kind.as_u16() as u32 == KIND_INTERACTION_RESPONSE));
    let state = f.state(&p).await;
    assert_eq!(state.votes[&alice_hex].source.id, alice_text.id);
    assert_eq!(state.summary(&Prompt::parse(&p).unwrap())["tally"]["no"], 1);
    // A reply to an unrelated message is an ordinary message.
    let unrelated = f.event(&f.asker, KIND_STREAM_MESSAGE, "hello", vec![]);
    let mut tx = f.db.begin_event_write_transaction().await.unwrap();
    store(&mut tx, f.community, f.channel, &unrelated, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let reply = f.event(
        &f.alice,
        KIND_STREAM_MESSAGE,
        "yes",
        vec![vec!["e", &unrelated.id.to_hex(), "", "reply"]],
    );
    assert!(f.accept(&reply).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn prompt_creation_is_rate_limited_per_key_and_capped_per_channel() {
    let f = Fixture::new().await;
    let make = |key: &Keys, n: usize| {
        f.event(
            key,
            KIND_INTERACTION_PROMPT,
            &format!("Question {n}"),
            vec![
                vec!["itype", "buttons"],
                vec!["opt", "yes", "Yes"],
                vec!["closes", "manual"],
                vec!["deadline", &(Timestamp::now().as_secs() + 3600).to_string()],
            ],
        )
    };
    for n in 0..10 {
        f.accept(&make(&f.asker, n)).await.unwrap();
    }
    let eleventh = make(&f.asker, 10);
    assert!(matches!(
        f.accept(&eleventh).await,
        Err(DbError::AccessDenied(_))
    ));
    assert!(!f.stored(&eleventh).await);
    // Other keys keep going until the channel holds 32 open prompts.
    let mut extra = Vec::new();
    for _ in 0..3 {
        let key = Keys::generate();
        f.add_member(&key).await;
        extra.push(key);
    }
    for (index, key) in extra.iter().enumerate() {
        let budget = if index == 2 { 2 } else { 10 };
        for n in 0..budget {
            f.accept(&make(key, 100 + index * 10 + n)).await.unwrap();
        }
    }
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM interactions WHERE community_id=$1 AND channel_id=$2 AND NOT closed",
    )
    .bind(f.community.as_uuid())
    .bind(f.channel)
    .fetch_one(&f.db.pool)
    .await
    .unwrap();
    assert_eq!(open, 32);
    let fresh = Keys::generate();
    f.add_member(&fresh).await;
    assert!(matches!(
        f.accept(&make(&fresh, 999)).await,
        Err(DbError::AccessDenied(_))
    ));
    // Closing one prompt frees a slot.
    let closer = &extra[2];
    let victim: Vec<u8> = sqlx::query_scalar(
        "SELECT prompt_id FROM interactions WHERE community_id=$1 AND author=$2 LIMIT 1",
    )
    .bind(f.community.as_uuid())
    .bind(closer.public_key().to_bytes().as_slice())
    .fetch_one(&f.db.pool)
    .await
    .unwrap();
    let victim_event: Event = serde_json::from_value(
        sqlx::query_scalar(
            "SELECT prompt FROM interactions WHERE community_id=$1 AND prompt_id=$2",
        )
        .bind(f.community.as_uuid())
        .bind(&victim)
        .fetch_one(&f.db.pool)
        .await
        .unwrap(),
    )
    .unwrap();
    f.accept(&f.close(closer, &victim_event)).await.unwrap();
    f.accept(&make(&fresh, 1000)).await.unwrap();
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn removed_outbox_backlog_cannot_escape_the_bounded_prune() {
    let f = Fixture::new().await;
    let p = f.prompt("first", "members");
    f.accept(&p).await.unwrap();
    let mut tx = f.db.begin_event_write_transaction().await.unwrap();
    let mut removed = Vec::new();
    for index in 0..105 {
        let event = f.event(
            &f.alice,
            KIND_STREAM_MESSAGE,
            &format!("removed {index}"),
            vec![],
        );
        store(&mut tx, f.community, f.channel, &event, None)
            .await
            .unwrap();
        removed.push(event.id.to_bytes().to_vec());
    }
    sqlx::query("UPDATE events SET deleted_at=now() WHERE community_id=$1 AND id=ANY($2)")
        .bind(f.community.as_uuid())
        .bind(&removed)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // One claim prunes at most 100 removed rows and never delivers one.
    let single = f.db.claim_interaction_events().await.unwrap();
    assert!(single
        .deliveries
        .iter()
        .all(|row| !removed.contains(&row.event.id.to_bytes().to_vec())));
    single.commit().await.unwrap();
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM interaction_outbox WHERE community_id=$1 AND event_id=ANY($2)",
    )
    .bind(f.community.as_uuid())
    .bind(&removed)
    .fetch_one(&f.db.pool)
    .await
    .unwrap();
    assert!(
        remaining >= 5,
        "the regression must exceed the 100-row pruning batch (remaining {remaining})"
    );
    // Later claims keep pruning the backlog while the live prompt still delivers.
    let (batch, ours) = f.claimed().await;
    batch.commit().await.unwrap();
    assert!(ours.iter().any(|row| row.event.id == p.id));
    assert!(ours
        .iter()
        .all(|row| !removed.contains(&row.event.id.to_bytes().to_vec())));
}
