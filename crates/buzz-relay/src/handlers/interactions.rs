//! One experimental gate shared by HTTP and WebSocket ingest.

use std::{sync::Arc, time::Duration};

use buzz_core::{kind::*, tenant::TenantContext, StoredEvent};
use nostr::{Event, Tag};

use super::ingest::{IngestError, IngestResult};
use crate::state::AppState;

/// Tags that assert relay provenance. Typed interactions reject them in
/// `buzz_core::interaction::validate_envelope`; ordinary messages must not
/// carry them either, or a client could dress a message up as a projection.
const RESERVED_PROVENANCE_TAGS: [&str; 3] = ["via", "actor", "interaction"];

pub(crate) fn check_enabled(enabled: bool, kind: u32) -> Result<(), IngestError> {
    if !enabled
        && matches!(
            kind,
            KIND_INTERACTION_PROMPT
                | KIND_INTERACTION_RESPONSE
                | KIND_INTERACTION_CLOSE
                | KIND_INTERACTION_STATE
        )
    {
        return Err(IngestError::Rejected(
            "restricted: experimental interactions are disabled".into(),
        ));
    }
    Ok(())
}

/// Reject client-signed messages that claim relay provenance.
pub(crate) fn check_reserved_tags(event: &Event) -> Result<(), IngestError> {
    let claims_provenance = event.tags.iter().map(Tag::as_slice).any(|t| {
        t.first()
            .is_some_and(|name| RESERVED_PROVENANCE_TAGS.contains(&name.as_str()))
    });
    if claims_provenance {
        return Err(IngestError::Rejected(
            "invalid: relay provenance tags are reserved".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn try_ingest(
    tenant: &TenantContext,
    state: &Arc<AppState>,
    event: &Event,
    meta: Option<buzz_db::event::ThreadMetadataParams<'_>>,
    actor_pubkey_hex: &str,
) -> Result<Option<IngestResult>, IngestError> {
    let kind = event_kind_u32(event);
    if !matches!(
        kind,
        KIND_INTERACTION_PROMPT
            | KIND_INTERACTION_RESPONSE
            | KIND_INTERACTION_CLOSE
            | KIND_STREAM_MESSAGE
            | KIND_STREAM_MESSAGE_V2
            | KIND_FORUM_COMMENT
    ) {
        return Ok(None);
    }
    if !matches!(
        kind,
        KIND_INTERACTION_PROMPT | KIND_INTERACTION_RESPONSE | KIND_INTERACTION_CLOSE
    ) {
        check_reserved_tags(event)?;
        if buzz_core::nip10::parse_thread_markers(&event.tags)
            .resolve()
            .is_none()
        {
            return Ok(None);
        }
    }
    let result = state
        .db
        .accept_interaction(tenant.community(), event, &state.relay_keypair, meta)
        .await
        .map_err(|e| match e {
            buzz_db::DbError::InvalidData(message) => {
                IngestError::Rejected(format!("invalid: {message}"))
            }
            buzz_db::DbError::AccessDenied(message) => {
                IngestError::Rejected(format!("restricted: {message}"))
            }
            other => IngestError::Internal(format!("error: accepting interaction: {other}")),
        })?;
    let Some(events) = result else {
        return Ok(None);
    };
    for stored in &events {
        let stored_kind = event_kind_u32(&stored.event);
        // The standard audit path records the authenticated actor once per
        // accepted event, at acceptance. Outbox retries and multi-pod delivery
        // must never append further audit entries for the same event.
        super::event::enqueue_event_created_audit(
            tenant,
            state,
            stored,
            stored_kind,
            actor_pubkey_hex,
            &stored.event.id.to_hex(),
        )
        .await;
        // A matched text answer remains an ordinary message, including its
        // existing workflow triggers. Run once on insertion, never on replays.
        if matches!(
            stored_kind,
            KIND_STREAM_MESSAGE | KIND_STREAM_MESSAGE_V2 | KIND_FORUM_COMMENT
        ) {
            super::event::trigger_event_workflows(tenant, state, stored, stored_kind);
        }
    }
    // No fire-and-forget dependency: accepted events are in the durable outbox.
    // The worker publishes them with tenant labels and the ordinary access gates.
    Ok(Some(IngestResult {
        event_id: event.id.to_hex(),
        accepted: true,
        message: if events.is_empty() {
            "duplicate".into()
        } else {
            String::new()
        },
    }))
}

/// Run bounded expiry and at-least-once Redis delivery. All failures retain
/// durable work; restart and multiple pods are safe (event IDs deduplicate).
pub async fn run_worker(state: Arc<AppState>) {
    let mut ticks = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        ticks += 1;
        if ticks.is_multiple_of(5) {
            match state.db.expire_interactions(&state.relay_keypair).await {
                Ok(closed) => {
                    let relay_hex = state.relay_keypair.public_key().to_hex();
                    for row in closed {
                        let tenant = TenantContext::resolved(row.community, row.host);
                        let stored = StoredEvent::new(row.event, Some(row.channel));
                        super::event::enqueue_event_created_audit(
                            &tenant,
                            &state,
                            &stored,
                            event_kind_u32(&stored.event),
                            &relay_hex,
                            &stored.event.id.to_hex(),
                        )
                        .await;
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "interaction expiry failed; will retry");
                }
            }
        }
        if let Err(error) = deliver_pending(&state).await {
            tracing::error!(%error, "interaction delivery failed; durable outbox retained");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}

/// Publish one claimed batch. A publication failure commits the
/// acknowledgements made so far and leaves the rest claimed by nobody, so the
/// next tick (on any pod) retries from the first unpublished event.
async fn deliver_pending(state: &Arc<AppState>) -> anyhow::Result<()> {
    let mut batch = state.db.claim_interaction_events().await?;
    let deliveries = std::mem::take(&mut batch.deliveries);
    let mut failure = None;
    for row in deliveries {
        let tenant = TenantContext::resolved(row.community, row.host);
        if let Err(error) = state
            .pubsub
            .publish_event(
                &tenant,
                buzz_pubsub::EventTopic::Channel(row.channel),
                &row.event,
            )
            .await
        {
            failure = Some(anyhow::Error::from(error));
            break;
        }
        batch.acknowledge(row.community, &row.event).await?;
    }
    batch.commit().await?;
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind};

    #[test]
    fn experimental_gate_is_closed_for_every_new_write_kind_only() {
        for k in [
            KIND_INTERACTION_PROMPT,
            KIND_INTERACTION_RESPONSE,
            KIND_INTERACTION_CLOSE,
            KIND_INTERACTION_STATE,
        ] {
            assert!(check_enabled(false, k).is_err());
            assert!(check_enabled(true, k).is_ok());
        }
        for k in [KIND_STREAM_MESSAGE, KIND_REACTION, KIND_APPROVAL_GRANT] {
            assert!(check_enabled(false, k).is_ok());
        }
        assert!(is_relay_only_kind(KIND_INTERACTION_STATE));
    }

    #[test]
    fn ordinary_messages_cannot_claim_relay_provenance() {
        let keys = Keys::generate();
        let message = |tags: Vec<Tag>| {
            EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE as u16), "approve")
                .tags(tags)
                .sign_with_keys(&keys)
                .unwrap()
        };
        let h = Tag::parse(["h", "00000000-0000-0000-0000-000000000001"]).unwrap();
        assert!(check_reserved_tags(&message(vec![h.clone()])).is_ok());
        assert!(check_reserved_tags(&message(vec![
            h.clone(),
            Tag::parse(["e", &"a".repeat(64), "", "reply"]).unwrap(),
            Tag::parse(["expiration", "1"]).unwrap(),
        ]))
        .is_ok());
        for reserved in RESERVED_PROVENANCE_TAGS {
            let forged = message(vec![
                h.clone(),
                Tag::parse([reserved, &"b".repeat(64)]).unwrap(),
            ]);
            let error = check_reserved_tags(&forged).unwrap_err();
            assert!(
                matches!(error, IngestError::Rejected(ref m) if m.starts_with("invalid:")),
                "{reserved}: {error:?}"
            );
        }
    }
}
