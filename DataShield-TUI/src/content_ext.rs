//! The Memorandum and item-index accessors on [`VaultContent`], as an
//! extension trait: they live in the TUI because `memo` and `rag` do (both
//! are app-private side tables under `extra`), while `VaultContent` itself is
//! the shared `datashield_core` model every DataShield front-end reads.

use crate::model::VaultContent;

pub trait VaultContentExt {
    fn memo_messages(&self) -> Vec<crate::memo::MemoMessage>;
    fn memo_len(&self) -> usize;
    fn memo_rows(&self) -> Vec<crate::memo::MemoRow<'_>>;
    fn push_memo_message(&mut self, msg: crate::memo::MemoMessage);
    fn pop_memo_message(&mut self) -> bool;
    fn set_memo_vec(
        &mut self,
        id: &str,
        embedding: &[f32],
        fingerprint: &str,
    ) -> Option<crate::memo::VecSlot>;
    fn restore_memo_vec(&mut self, slot: &crate::memo::VecSlot) -> bool;
    fn item_vectors(&self) -> crate::rag::ItemVectors;
    fn set_item_vectors(
        &mut self,
        tag: &str,
        vectors: &[crate::rag::Embedded],
        live: &[String],
    ) -> crate::rag::RagSlot;
    fn restore_item_vectors(&mut self, slot: crate::rag::RagSlot);
}

impl VaultContentExt for VaultContent {
    // ---- the Memorandum (app-private, under `x_memo`) ---------------------
    //
    // Thin typed accessors over [`crate::memo`], which owns the wire shape.
    // Reading is tolerant (an unreadable message is skipped); writing edits the
    // stored json in place, so nothing a reader couldn't parse is ever dropped.

    /// The whole conversation, oldest first, minus any message this build
    /// cannot read (still on disk, just not returned).
    fn memo_messages(&self) -> Vec<crate::memo::MemoMessage> {
        crate::memo::messages(&self.extra)
    }

    /// How many messages are stored — without deep-copying their photos.
    fn memo_len(&self) -> usize {
        crate::memo::len(&self.extra)
    }

    /// The same conversation as a SEARCH reads it: borrowed from the stored
    /// json, photos left where they are. What a reader that only ranks words
    /// asks for — [`memo_messages`](VaultContent::memo_messages) is what one
    /// that draws a picture asks for.
    fn memo_rows(&self) -> Vec<crate::memo::MemoRow<'_>> {
        crate::memo::rows(&self.extra)
    }

    /// Appends one message. The caller then bumps the sync counter and saves,
    /// like every other content mutation;
    /// [`pop_memo_message`](VaultContent::pop_memo_message) is its rollback.
    fn push_memo_message(&mut self, msg: crate::memo::MemoMessage) {
        crate::memo::push(&mut self.extra, msg);
    }

    /// Removes the last message — the exact inverse of a push (a rolled-back
    /// append restores the content fingerprint). Returns whether one went.
    fn pop_memo_message(&mut self) -> bool {
        crate::memo::pop(&mut self.extra)
    }

    /// Attaches (or replaces) the embedding of the message with `id` and the
    /// fingerprint of the text it came from, in place. `None` when there is no
    /// such message or the vector isn't finite; otherwise the slot it replaced,
    /// which [`restore_memo_vec`](VaultContent::restore_memo_vec) puts back.
    fn set_memo_vec(
        &mut self,
        id: &str,
        embedding: &[f32],
        fingerprint: &str,
    ) -> Option<crate::memo::VecSlot> {
        crate::memo::set_vec(&mut self.extra, id, embedding, fingerprint)
    }

    /// Undoes one [`set_memo_vec`](VaultContent::set_memo_vec) — the rollback
    /// for an index batch whose save was refused.
    fn restore_memo_vec(&mut self, slot: &crate::memo::VecSlot) -> bool {
        crate::memo::restore_vec(&mut self.extra, slot)
    }

    // ---- the ITEM index (app-private, under `x_rag`) ----------------------
    //
    // The same contract as the Memorandum's vectors, in a side table rather
    // than on each item: an item is DCS-1's own shape and other DataShield
    // readers rewrite it, while `x_rag` is ours and round-trips untouched.
    // Only NON-SECRET text is ever embedded — [`crate::rag::indexable_fields`]
    // is what enforces it, and it is the only door into the text builder.

    /// The stored item vectors (empty when nothing has been indexed yet).
    fn item_vectors(&self) -> crate::rag::ItemVectors {
        crate::rag::read_item_vectors(&self.extra)
    }

    /// Merges a finished batch in and prunes the vectors of items that are
    /// gone. `live` is the id set of the items that still exist; the returned
    /// slot is the rollback for a save that fails.
    fn set_item_vectors(
        &mut self,
        tag: &str,
        vectors: &[crate::rag::Embedded],
        live: &[String],
    ) -> crate::rag::RagSlot {
        crate::rag::set_item_vectors(&mut self.extra, tag, vectors, live)
    }

    /// Undoes one [`set_item_vectors`](VaultContent::set_item_vectors).
    fn restore_item_vectors(&mut self, slot: crate::rag::RagSlot) {
        crate::rag::restore_item_vectors(&mut self.extra, slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn the_fingerprint_covers_the_memorandum_but_not_the_sync_stamp() {
        let mut content = VaultContent::default();
        let empty = content.content_fingerprint();
        // the stamp is metadata: bumping it is not a content change…
        content.bump_sync_counter("local");
        assert_eq!(content.content_fingerprint(), empty, "x_sync is not content");
        // …a memo message is, or an equal-counter sync would call two different
        // conversations "up to date"
        content.push_memo_message(crate::memo::MemoMessage::new("remember the milk", None));
        let with_memo = content.content_fingerprint();
        assert_ne!(with_memo, empty, "a memo append diverges the content");
        // a second one diverges again, and undoing it restores the digest
        content.push_memo_message(crate::memo::MemoMessage::new("and the bread", None));
        assert_ne!(content.content_fingerprint(), with_memo);
        assert!(content.pop_memo_message());
        assert_eq!(content.content_fingerprint(), with_memo, "the rollback is exact");
        // an embedding attached later is content too (it rides the message)
        let id = content.memo_messages()[0].id.clone();
        let slot = content.set_memo_vec(&id, &[0.5, -0.25], "fp").expect("attached");
        assert_ne!(content.content_fingerprint(), with_memo);
        // …and taking it back off restores the digest, which is what makes a
        // refused index save safe to roll back
        assert!(content.restore_memo_vec(&slot));
        assert_eq!(content.content_fingerprint(), with_memo);
    }

    #[test]
    fn a_memo_message_round_trips_through_the_whole_content_document() {
        // the Memorandum travels with the vault: it is a plain `x_` member of
        // the flatten map, so an export/sync carries it and an older reader
        // rewrites it untouched.
        let mut content = VaultContent::default();
        content.push_memo_message(crate::memo::MemoMessage::new("first", None));
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""x_memo""#), "stored under the app-private key");
        let read: VaultContent = serde_json::from_str(&json).unwrap();
        assert_eq!(read.memo_len(), 1);
        assert_eq!(read.memo_messages()[0].text, "first");
        assert_eq!(read.schema_version, CONTENT_SCHEMA_VERSION, "no schema bump");
    }
}
