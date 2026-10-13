//! The replay cache's snapshot rules (Go antigravity_reasoning_replay_cache_test.go,
//! in-process cases) and the purge and capacity behavior, each on a private store.

use super::*;

fn item(signature: &str) -> Vec<u8> {
    format!(r#"{{"type":"thought_signature","contentIndex":1,"partIndex":0,"thoughtSignature":"{signature}"}}"#)
        .into_bytes()
}

fn items(signatures: &[&str]) -> Vec<Vec<u8>> {
    normalize(&signatures.iter().map(|s| item(s)).collect::<Vec<_>>()).unwrap()
}

fn min(n: u64) -> Duration {
    Duration::from_secs(60 * n)
}

#[test]
fn normalizes_signatures_and_native_calls() {
    let raw = vec![
        br#"{"type":"thought_signature","thoughtSignature":" sig-aaaaaaaaaaaaaaaa ","targetKind":"text","targetHash":"h","extra":1}"#.to_vec(),
        br#"{"type":"thought_signature","thoughtSignature":"short"}"#.to_vec(),
        br#"{"type":"other","thoughtSignature":"sig-bbbbbbbbbbbbbbbb"}"#.to_vec(),
        br#"{"type":"function_call_part","functionCall":{"id":"c1","name":"f","args":{"b":1,"a":[2]}},"thoughtSignature":"skip_thought_signature_validator","contentIndex":3,"targetOccurrence":-1,"contextHash":" ctx "}"#.to_vec(),
        br#"{"type":"function_call_part","name":"g","args":"{\"x\":1}","call_id":" c2 ","thoughtSignature":"sig","partIndex":2,"targetOccurrence":0,"targetKind":"text"}"#.to_vec(),
        br#"{"type":"function_call_part","name":"no-args"}"#.to_vec(),
    ];
    assert_eq!(
        normalize(&raw).unwrap(),
        vec![
            br#"{"type":"thought_signature","thoughtSignature":"sig-aaaaaaaaaaaaaaaa","targetKind":"text","targetHash":"h"}"#.to_vec(),
            br#"{"type":"function_call_part","call_id":"c1","name":"f","args":{"b":1,"a":[2]},"contentIndex":3,"contextHash":"ctx"}"#.to_vec(),
            br#"{"type":"function_call_part","call_id":"c2","name":"g","args":"{\"x\":1}","thoughtSignature":"sig","partIndex":2,"targetOccurrence":0}"#.to_vec(),
        ]
    );
    assert!(normalize(&raw[1..3]).is_none());
}

/// TestAntigravityReasoningReplayConditionalMutationRejectsStaleLocalSnapshot.
#[test]
fn stale_snapshots_neither_replace_nor_delete() {
    let (mut c, t0) = (Caches::default(), Instant::now());
    c.put("k".into(), items(&["old-local-signature-123456"]), t0);
    let (_, snapshot) = c.get("k".into(), t0);
    c.put("k".into(), items(&["new-local-signature-123456"]), t0);
    assert!(!c.replace("k".into(), &snapshot, items(&["stale-local-signature-123456"]), t0));
    assert!(!c.delete_if_unchanged("k".into(), &snapshot, t0));
    assert_eq!(c.get("k".into(), t0).0.unwrap(), items(&["new-local-signature-123456"]));
}

/// TestAntigravityReasoningReplayNonPrefixReplaceRotatesLocalBranch and
/// TestAntigravityReasoningReplayConditionalReplaceAcceptsDescendantLocalChain.
#[test]
fn descendants_extend_a_branch_and_resets_rotate_it() {
    let (mut c, t0) = (Caches::default(), Instant::now());
    c.put("k".into(), items(&["non-prefix-old-signature-123456"]), t0);
    let (_, first) = c.get("k".into(), t0);
    let (_, stale) = c.get("k".into(), t0);
    let new = items(&["non-prefix-new-signature-123456"]);
    assert!(c.replace("k".into(), &first, new.clone(), t0));
    let mut latest = new.clone();
    latest.extend(items(&["non-prefix-latest-signature-123456"]));
    assert!(
        !c.replace("k".into(), &stale, latest, t0),
        "stale descendant crossed a reset"
    );

    let (prefix, middle, last) = (
        "descendant-prefix-signature-123456",
        "descendant-middle-signature-123456",
        "descendant-latest-signature-123456",
    );
    c.put("d".into(), items(&[prefix]), t0);
    let (_, stale) = c.get("d".into(), t0);
    let (_, first) = c.get("d".into(), t0);
    assert!(c.replace("d".into(), &first, items(&[prefix, middle]), t0));
    assert!(c.replace("d".into(), &stale, items(&[prefix, middle, last]), t0));
    assert_eq!(c.get("d".into(), t0).0.unwrap().len(), 3);
}

/// TestAntigravityReasoningReplayDescendantMergeRejectsResetBranchABA.
#[test]
fn a_reset_branch_rejects_old_descendants() {
    let (mut c, t0) = (Caches::default(), Instant::now());
    let (prefix, middle) = ("reset-prefix-signature-123456", "reset-middle-signature-123456");
    c.put("k".into(), items(&[prefix]), t0);
    let (_, stale) = c.get("k".into(), t0);
    let (_, first) = c.get("k".into(), t0);
    assert!(c.replace("k".into(), &first, items(&[prefix, middle]), t0));
    let (_, current) = c.get("k".into(), t0);
    assert!(c.delete_if_unchanged("k".into(), &current, t0));
    let (_, reset) = c.get("k".into(), t0);
    assert!(c.replace("k".into(), &reset, items(&[prefix]), t0));
    assert!(!c.replace("k".into(), &stale, items(&[prefix, "reset-stale-signature-123456"]), t0));
}

/// TestAntigravityReasoningReplayConditionalDeleteTombstoneBlocksStaleFirstWriter,
/// ...EvictedTombstoneStillBlocksStaleFirstWriter and
/// ...UnrelatedEvictionDoesNotBlockAbsentSnapshot.
#[test]
fn absent_snapshots_are_fenced() {
    let (mut c, t0) = (Caches::default(), Instant::now());
    let (none, stale) = c.get("k".into(), t0);
    assert!(none.is_none());
    let (_, clear) = c.get("k".into(), t0);
    assert!(c.delete_if_unchanged("k".into(), &clear, t0));
    assert!(!c.replace("k".into(), &stale, items(&["stale-first-writer-signature-123456"]), t0));

    let (mut c, t0) = (Caches::default(), Instant::now());
    let (_, stale) = c.get("k".into(), t0);
    let (_, clear) = c.get("k".into(), t0);
    assert!(c.delete_if_unchanged("k".into(), &clear, t0));
    c.evict_oldest();
    assert!(c.replay.is_empty());
    assert!(!c.replace(
        "k".into(),
        &stale,
        items(&["evicted-stale-first-writer-signature-123456"]),
        t0
    ));

    let (mut c, t0) = (Caches::default(), Instant::now());
    c.put("older".into(), items(&["evicted-live-signature-123456"]), t0);
    let (_, snapshot) = c.get("untouched".into(), t0 + min(1));
    // Only the older entry goes (the batch is larger than the map here).
    c.replay.remove("older");
    c.eviction_epoch += 1;
    assert!(c.replace(
        "untouched".into(),
        &snapshot,
        items(&["first-write-after-unrelated-eviction-123456"]),
        t0
    ));
}

#[test]
fn misses_reserve_tombstones_that_count_toward_capacity() {
    let (mut c, t0) = (Caches::default(), Instant::now());
    c.put("a".into(), vec![b"item".to_vec()], t0);
    // 10,239 distinct misses fill the map; the next one evicts the oldest 128, `a` first.
    for i in 1..MAX_ENTRIES {
        assert!(
            c.get(format!("absent-{i}"), t0 + Duration::from_micros(i as u64))
                .0
                .is_none()
        );
    }
    assert_eq!(c.replay.len(), MAX_ENTRIES);
    assert_eq!(
        c.get("a".into(), t0 + min(1)).0.as_deref(),
        Some(&[b"item".to_vec()][..])
    );
    c.get("absent-last".into(), t0 + min(2));
    assert_eq!(c.replay.len(), MAX_ENTRIES - EVICT_BATCH + 1);
    assert!(c.replay.contains_key("a"), "refreshed by the hit");
    assert!(!c.replay.contains_key("absent-1"));
    // A tombstone hit stays a miss and refreshes the tombstone.
    assert!(c.get("absent-last".into(), t0 + min(3)).0.is_none());
    assert_eq!(c.replay["absent-last"].at, t0 + min(3));
}

/// TestAntigravityReasoningReplayLocalTombstonesStayWithinEntryBound.
#[test]
fn tombstones_stay_within_the_entry_bound() {
    let (mut c, t0) = (Caches::default(), Instant::now());
    for i in 0..=MAX_ENTRIES {
        c.delete_if_unchanged(
            format!("tombstone-{i}"),
            &ReplaySnapshot::default(),
            t0 + Duration::from_micros(i as u64),
        );
    }
    assert!(c.replay.len() <= MAX_ENTRIES);
}

#[test]
fn expired_entries_purge_at_go_tick_times() {
    let (mut c, t0) = (Caches::default(), Instant::now());
    c.put("first".into(), vec![b"i".to_vec()], t0); // starts the ticker
    c.put("x".into(), vec![b"i".to_vec()], t0 + min(5));
    // Tick 6 (t0+60m): `first` is exactly 60m old (Go deletes only past the TTL) and
    // `x` 55m old, so both stay, although `x` is 61m old now.
    c.get("y".into(), t0 + min(66));
    assert!(c.replay.contains_key("x"));
    assert!(c.replay.contains_key("first"));
    c.get("z".into(), t0 + min(70) + Duration::from_secs(1));
    assert!(!c.replay.contains_key("x") && !c.replay.contains_key("first"));
    assert!(c.replay.contains_key("y") && c.replay.contains_key("z"));
}

#[test]
fn signature_groups_expire_and_reads_do_not_start_the_ticker() {
    let (mut c, t0) = (Caches::default(), Instant::now());
    assert!(c.cached_signature("claude", "h", t0).is_none());
    assert!(c.ticker.is_none());
    let sig = b"\xff raw bytes".to_vec();
    c.cache_signature("claude", "h".into(), &sig, t0 + min(5));
    c.cache_signature("gemini", "g".into(), &sig, t0 + min(5));
    assert_eq!(c.ticker, Some((t0 + min(5), 0)));
    assert_eq!(c.cached_signature("claude", "h", t0 + min(100)), Some(sig.clone()));
    // gemini/g is 180m old at tick 18 (t0+185m): kept; past the TTL at tick 19.
    c.cached_signature("claude", "none", t0 + min(186));
    assert!(c.signatures.contains_key("gemini"));
    c.cached_signature("claude", "none", t0 + min(196));
    assert!(!c.signatures.contains_key("gemini"), "empty group removed");
    assert!(c.signatures.contains_key("claude"), "refreshed at t0+100m");
}
