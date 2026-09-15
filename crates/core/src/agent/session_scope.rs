//! Which events belong to the session the user is looking at.
//!
//! Every backend that carries more than one session down a single connection has
//! to answer this, and getting it wrong is not a visible failure: another
//! session's reply is appended to whatever chat happens to be open, and the user
//! reads someone else's conversation as their own. A Gateway carries every
//! session down one connection and subscribing to a session does not unsubscribe
//! from the ones opened before it, so a cron job in another session arrives on
//! the same socket as the reply being typed.
//!
//! This is one rule, so it lives in one place and every adapter calls it. An
//! adapter that gains a session-labelled stream must call it too — see
//! `docs/sessions.md`.

/// Whether an event labelled `event_session` belongs to the session this client
/// subscribed to.
///
/// `subscribed` holds every id the client knows that session by: a backend may
/// name it by the live id in one place and the stored key in another, and
/// refusing the subscriber's own traffic is worse than the leak — it looks
/// exactly like a session that never answers.
///
/// The rule, in the order the questions come up:
///
/// * **Nothing subscribed yet** — keep everything. There is nothing to compare
///   against, and the event that announces the session is one of these.
/// * **An unlabelled event** — keep it. A backend that labels the traffic it
///   fans out may leave its own single-session stream unlabelled, so dropping
///   these would lose the active session's own reply.
/// * **A labelled event** — keep it only when it names the subscribed session.
pub fn belongs_to_session<'a>(
    subscribed: impl IntoIterator<Item = &'a str>,
    event_session: Option<&str>,
) -> bool {
    let mut subscribed = subscribed.into_iter();
    let Some(first) = subscribed.next() else {
        return true;
    };
    match event_session {
        None => true,
        Some(event) => first == event || subscribed.any(|id| id == event),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_labelled_event_for_the_subscribed_session_is_kept() {
        assert!(belongs_to_session(
            ["agent:main:one"],
            Some("agent:main:one")
        ));
    }

    #[test]
    fn a_labelled_event_for_another_session_is_dropped() {
        assert!(!belongs_to_session(
            ["agent:main:one"],
            Some("agent:main:other"),
        ));
    }

    /// Before a session is open there is nothing to compare against — and the
    /// event that names the session is itself one of these.
    #[test]
    fn nothing_is_filtered_before_a_session_is_subscribed() {
        assert!(belongs_to_session([], Some("agent:main:any")));
        assert!(belongs_to_session([], None));
        assert!(belongs_to_session(std::iter::empty(), None));
    }

    /// Dropping an unlabelled frame could lose the active session's own stream,
    /// which is the worse of the two failures.
    #[test]
    fn an_unlabelled_event_is_kept() {
        assert!(belongs_to_session(["agent:main:one"], None));
    }

    /// A backend may name a session by its live id in the event stream and by its
    /// stored key when subscribing; both are the session on screen, and refusing
    /// either would look like a session that never answers.
    #[test]
    fn every_id_the_client_knows_the_session_by_is_accepted() {
        assert!(belongs_to_session(["live-1", "stored-1"], Some("live-1")));
        assert!(belongs_to_session(["live-1", "stored-1"], Some("stored-1")));
        assert!(!belongs_to_session(["live-1", "stored-1"], Some("live-2")));
    }
}
