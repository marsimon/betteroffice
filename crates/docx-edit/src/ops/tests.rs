use super::*;
use crate::TableLocator;
use crate::op::{loc_in_bounds, loc_range_in_txn, para_bounds};
use crate::{EditCtx, EditingDoc, FormatPolicy, Position, RawOp, SeedParagraph};
use yrs::Transact;

fn mixed_doc() -> EditingDoc {
    let doc = EditingDoc::new(7);
    crate::segments::tests::seed_text_stream(&doc);
    let mut txn = doc.yrs_doc().transact_mut();
    let story = crate::story_ref(&txn, "body").unwrap();
    story.insert_embed(&mut txn, 2, true);
    story.format(
        &mut txn,
        0,
        1,
        Attrs::from([("hyperlink".into(), Any::from("https://example.org"))]),
    );
    for (index, _) in crate::pilcrows(&story, &txn) {
        story.format(
            &mut txn,
            index,
            1,
            Attrs::from([("italic".into(), Any::Bool(true))]),
        );
    }
    drop(txn);
    doc
}

fn assert_chunks_eq(actual: &[Chunk], expected: &[Chunk]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!((actual.start, actual.len), (expected.start, expected.len));
        assert_eq!(actual.attrs, expected.attrs);
        match (&actual.kind, &expected.kind) {
            (ChunkKind::Text(actual), ChunkKind::Text(expected)) => assert_eq!(actual, expected),
            (ChunkKind::Pilcrow(actual), ChunkKind::Pilcrow(expected)) => {
                assert_eq!(actual, expected)
            }
            (ChunkKind::Embed(actual), ChunkKind::Embed(expected)) => assert_eq!(actual, expected),
            _ => panic!("chunk kind differs"),
        }
    }
}

fn assert_windows(doc: &EditingDoc, story_id: &str) {
    doc.segment_index(story_id).unwrap();
    let txn = doc.yrs_doc().transact_mut();
    let story = crate::story_ref(&txn, story_id).unwrap();
    let indexed = IndexedStory::in_txn(doc, story_id, &txn).unwrap();
    let len = story.len(&txn);
    let bounds = para_bounds(&story, &txn);
    for lo in 0..=len + 1 {
        for hi in lo..=len + 1 {
            for through_embeds in [false, true] {
                let expected = snapshot_window(&story, &txn, lo, hi, through_embeds);
                let actual = indexed
                    .snapshot_window(&txn, lo, hi, through_embeds)
                    .unwrap();
                assert_chunks_eq(&actual, &expected);
            }
            let mut expected_range = StoryRange::new(story_id, lo, hi);
            let mut actual_range = expected_range.clone();
            let (expected, expected_single) =
                range_chunks_observed(&story, &txn, &mut expected_range);
            let (actual, actual_single) =
                range_chunks_observed_indexed(&story, &txn, &mut actual_range, Some(&indexed));
            assert_eq!(actual_range, expected_range);
            assert_eq!(actual_single, expected_single);
            assert_chunks_eq(&actual, &expected);
        }
        let mut expected_index = lo;
        let mut actual_index = lo;
        let expected = position_chunks(&story, &txn, &mut expected_index);
        let actual = position_chunks_indexed(&story, &txn, &mut actual_index, Some(&indexed));
        assert_eq!(actual_index, expected_index);
        assert_chunks_eq(&actual, &expected);
        assert_eq!(
            past_leading_blocks_indexed(&story, &txn, lo, Some(&indexed)),
            past_leading_blocks(&story, &txn, lo)
        );
        if let Some(actual) = indexed.bounds_at(&txn, lo) {
            assert_eq!(
                loc_in_bounds(story_id, &[actual], lo),
                loc_in_bounds(story_id, &bounds, lo)
            );
        } else {
            assert!(bounds.is_empty());
        }
    }
}

#[test]
fn indexed_windows_match_diff_at_every_position_and_range() {
    assert_windows(&mixed_doc(), "body");
    let doc = mixed_doc();
    {
        let mut txn = doc.yrs_doc().transact_mut();
        let story = crate::story_ref(&txn, "body").unwrap();
        let end = story.len(&txn);
        story.insert(&mut txn, end, "tail😀");
    }
    assert_windows(&doc, "body");
    let doc = EditingDoc::new(8);
    doc.create_story("body", "", "Normal", "left").unwrap();
    assert_windows(&doc, "body");
    {
        let mut txn = doc.yrs_doc().transact_mut();
        let story = crate::story_ref(&txn, "body").unwrap();
        story.remove_range(&mut txn, 0, 1);
    }
    assert_windows(&doc, "body");
}

#[test]
fn indexed_windows_match_diff_in_related_stories() {
    let doc = mixed_doc();
    for story in ["hf:rId1", "hf:rId2", "fn:1", "en:2"] {
        doc.seed_story(
            story,
            &["a😀é", "", "tail"].map(|text| SeedParagraph {
                text: text.into(),
                p_style: "Normal".into(),
                alignment: "left".into(),
            }),
        )
        .unwrap();
        doc.apply_raw_ops(
            story,
            vec![RawOp::InsertEmbed {
                index: 0,
                kind: "blockSdt".into(),
                payload: Vec::new(),
                attrs: Attrs::from([("italic".into(), Any::Bool(true))]),
            }],
            &EditCtx::local("", ""),
        )
        .unwrap();
        assert_windows(&doc, story);
        assert_related_story_edits(&doc, story);
    }
    let table = doc
        .insert_table(&EditCtx::local("", ""), Position::new("hf:rId1", 0), 1, 2)
        .unwrap();
    for story in table.created_story_ids {
        doc.insert_text(
            &EditCtx::local("", ""),
            Position::new(&story, 0),
            "cell😀",
            FormatPolicy::Inherit,
        )
        .unwrap();
        assert_windows(&doc, &story);
        assert_related_story_edits(&doc, &story);
    }
    assert_windows(&doc, "hf:rId1");
}

fn replica(state: &[u8], indexed: bool) -> EditingDoc {
    let doc = EditingDoc::new(91);
    doc.apply_update_v1(state).unwrap();
    if indexed {
        doc.segment_index("body").unwrap();
        assert!(IndexedStory::in_txn(&doc, "body", &doc.yrs_doc().transact_mut()).is_some());
        doc.segment_index("body").unwrap();
    }
    doc
}

fn assert_related_story_edits(doc: &EditingDoc, story: &str) {
    let state = doc.encode_state_as_update_v1();
    let len = doc.story_len(story).unwrap();
    let ctx = EditCtx::local("", "");
    for at in 0..=len {
        let actual = replica(&state, false);
        actual.segment_index(story).unwrap();
        assert!(IndexedStory::in_txn(&actual, story, &actual.yrs_doc().transact_mut()).is_some());
        actual.segment_index(story).unwrap();
        let expected = replica(&state, false);
        assert_eq!(
            actual.insert_text(&ctx, Position::new(story, at), "x", FormatPolicy::Inherit),
            expected.insert_text(&ctx, Position::new(story, at), "x", FormatPolicy::Inherit)
        );
        assert_eq!(actual.story_segments(story), expected.story_segments(story));
        if at < len {
            let actual = replica(&state, false);
            actual.segment_index(story).unwrap();
            let expected = replica(&state, false);
            assert_eq!(
                actual.delete_range(&ctx, StoryRange::new(story, at, at + 1)),
                expected.delete_range(&ctx, StoryRange::new(story, at, at + 1))
            );
            assert_eq!(actual.story_segments(story), expected.story_segments(story));
        }
    }
}

#[test]
fn indexed_text_edits_match_diff_receipts_and_formatting() {
    let doc = mixed_doc();
    for author in ["writer", "other"] {
        doc.insert_text(
            &EditCtx::local(author, "").suggesting(),
            Position::new("body", 1),
            "xy",
            FormatPolicy::Inherit,
        )
        .unwrap();
    }
    let state = doc.encode_state_as_update_v1();
    let len = doc.story_len("body").unwrap();
    for ctx in [
        EditCtx::local("writer", ""),
        EditCtx::local("writer", "").suggesting(),
    ] {
        for start in 0..=len {
            for policy in [
                FormatPolicy::Inherit,
                FormatPolicy::Plain,
                FormatPolicy::Explicit(BTreeMap::from([("italic".into(), Any::Bool(true))])),
            ] {
                for text in ["", "é😀"] {
                    let actual = replica(&state, true);
                    let expected = replica(&state, false);
                    assert_eq!(
                        actual.insert_text(
                            &ctx,
                            Position::new("body", start),
                            text,
                            policy.clone()
                        ),
                        expected.insert_text(
                            &ctx,
                            Position::new("body", start),
                            text,
                            policy.clone()
                        )
                    );
                    assert_eq!(
                        actual.story_segments("body"),
                        expected.story_segments("body")
                    );
                }
            }
            for end in start + 1..=len {
                let actual = replica(&state, true);
                let expected = replica(&state, false);
                assert_eq!(
                    actual.delete_range(&ctx, StoryRange::new("body", start, end)),
                    expected.delete_range(&ctx, StoryRange::new("body", start, end))
                );
                assert_eq!(
                    actual.story_segments("body"),
                    expected.story_segments("body")
                );
            }
        }
    }
}

#[test]
fn indexed_windows_match_diff_after_formatted_cache_advances() {
    let doc = mixed_doc();
    let ctx = EditCtx::local("", "");
    for step in 0..20 {
        doc.segment_index("body").unwrap();
        let before = doc.committed_epoch();
        let mut attrs = Attrs::new();
        doc.insert_text_observed(
            &ctx,
            Position::new("body", 1),
            if step % 2 == 0 { "é😀" } else { "x" },
            FormatPolicy::Inherit,
            |effective| attrs = effective.clone(),
        )
        .unwrap();
        doc.advance_indexes_after_formatted_text_insert(
            "body",
            before,
            doc.committed_epoch(),
            1,
            if step % 2 == 0 { "é😀" } else { "x" },
            Some(&attrs),
        );
        assert_windows(&doc, "body");
        let before = doc.committed_epoch();
        doc.delete_range(&ctx, StoryRange::new("body", 1, 2))
            .unwrap();
        doc.advance_indexes_after_text_delete("body", before, doc.committed_epoch(), 1, 2);
        assert_windows(&doc, "body");
    }
}

#[test]
fn indexed_reads_fall_back_for_missing_stale_and_modified_transaction_caches() {
    for missing in [
        "segments",
        "paragraphs",
        "epoch",
        "insert",
        "delete",
        "format",
        "map",
    ] {
        let doc = mixed_doc();
        doc.segment_index("body").unwrap();
        let epoch = doc.committed_epoch();
        match missing {
            "segments" => {
                doc.segment_indexes.lock().unwrap().take("body", epoch);
            }
            "paragraphs" => {
                doc.paragraph_indexes.lock().unwrap().take("body", epoch);
            }
            "epoch" => {
                doc.insert_text(
                    &EditCtx::local("", ""),
                    Position::new("body", 0),
                    "x",
                    FormatPolicy::Inherit,
                )
                .unwrap();
            }
            _ => {}
        }
        let mut txn = doc.yrs_doc().transact_mut();
        let story = crate::story_ref(&txn, "body").unwrap();
        match missing {
            "insert" => story.insert(&mut txn, 0, "x"),
            "delete" => story.remove_range(&mut txn, 0, 1),
            "format" => story.format(
                &mut txn,
                0,
                1,
                Attrs::from([("italic".into(), Any::Bool(true))]),
            ),
            "map" => {
                let (_, map) = crate::next_pilcrow(&story, &txn, 0).unwrap();
                map.insert(&mut txn, PARA_ID, "changed");
            }
            _ => {}
        }
        assert!(
            IndexedStory::in_txn(&doc, "body", &txn).is_none(),
            "{missing}"
        );
        let mut actual_index = 1;
        let actual = position_chunks_indexed(&story, &txn, &mut actual_index, None);
        let mut expected_index = 1;
        let expected = position_chunks(&story, &txn, &mut expected_index);
        assert_eq!(actual_index, expected_index);
        assert_chunks_eq(&actual, &expected);
    }
}

#[test]
fn unobserved_cache_advances_do_not_supply_insertion_formatting() {
    let doc = mixed_doc();
    doc.segment_index("body").unwrap();
    let before = doc.committed_epoch();
    doc.insert_text(
        &EditCtx::local("", ""),
        Position::new("body", 1),
        "x",
        FormatPolicy::Plain,
    )
    .unwrap();
    doc.advance_indexes_after_text_insert("body", before, doc.committed_epoch(), 1, "x");
    let txn = doc.yrs_doc().transact_mut();
    assert!(IndexedStory::in_txn(&doc, "body", &txn).is_none());
}

#[test]
fn typing_and_backspace_use_local_chunks_without_story_diffs() {
    for count in [8, 2048] {
        let doc = EditingDoc::new(71);
        doc.seed_story(
            "body",
            &vec![
                SeedParagraph {
                    text: "abcd".into(),
                    p_style: "Normal".into(),
                    alignment: "left".into()
                };
                count
            ],
        )
        .unwrap();
        doc.segment_index("body").unwrap();
        let at = (count as u32 / 2) * 5 + 2;
        crate::SNAPSHOT_DIFFS_BUILT.set(0);
        crate::PILCROW_DIFFS_BUILT.set(0);
        crate::SNAPSHOT_CHUNKS_BUILT.set(0);
        crate::STORY_INDEX_UNITS_BUILT.set(0);
        let ctx = EditCtx::local("", "");
        for _ in 0..20 {
            let before = doc.committed_epoch();
            let mut attrs = Attrs::new();
            let receipt = doc
                .insert_text_observed(
                    &ctx,
                    Position::new("body", at),
                    "x",
                    FormatPolicy::Inherit,
                    |effective| attrs = effective.clone(),
                )
                .unwrap();
            doc.advance_indexes_after_formatted_text_insert(
                "body",
                before,
                doc.committed_epoch(),
                at,
                "x",
                Some(&attrs),
            );
            let before = doc.committed_epoch();
            let deletion = doc
                .delete_range(&ctx, StoryRange::new("body", at, at + 1))
                .unwrap();
            doc.advance_indexes_after_text_delete(
                "body",
                before,
                doc.committed_epoch(),
                at,
                at + 1,
            );
            assert_eq!(receipt.range.unwrap().start, deletion.range.unwrap().start);
        }
        assert_eq!(crate::SNAPSHOT_DIFFS_BUILT.get(), 0);
        assert_eq!(crate::PILCROW_DIFFS_BUILT.get(), 0);
        assert!(crate::SNAPSHOT_CHUNKS_BUILT.get() <= 160);
        assert!(crate::STORY_INDEX_UNITS_BUILT.get() <= 160);
        let txn = doc.yrs_doc().transact();
        let story = crate::story_ref(&txn, "body").unwrap();
        let expected = loc_range_in_txn("body", &story, &txn, at, at).unwrap();
        let indexed_bounds = doc.paragraph_index("body").unwrap();
        assert_eq!(
            expected.start.offset,
            at - indexed_bounds.para_at(at).unwrap().start
        );
    }
}

#[test]
fn paragraph_edits_copy_only_the_selected_paragraphs() {
    let doc = EditingDoc::new(73);
    doc.seed_story(
        "body",
        &vec![
            SeedParagraph {
                text: "abcd".into(),
                p_style: "Normal".into(),
                alignment: "left".into()
            };
            64
        ],
    )
    .unwrap();
    let ids: Vec<String> = doc
        .paragraphs("body")
        .unwrap()
        .into_iter()
        .map(|paragraph| paragraph.para_id)
        .collect();
    let suggesting = EditCtx::local("Ann", "2026-10-21T00:00:00Z").suggesting();
    let inserted = doc
        .insert_text(
            &suggesting,
            Position::new("body", 40 * 5 + 2),
            "x",
            FormatPolicy::Inherit,
        )
        .unwrap()
        .revision_ids;
    let selected_chunks = |indexes: &[usize]| {
        let txn = doc.yrs_doc().transact();
        let story = crate::story_ref(&txn, "body").unwrap();
        let bounds = para_bounds(&story, &txn);
        indexes
            .iter()
            .map(|&index| {
                let bounds = &bounds[index];
                snapshot_range(&story, &txn, bounds.start, bounds.pilcrow + 1).len()
            })
            .sum::<usize>()
    };
    let styles = crate::seed::StyleResolver::new(None);
    let mut delta = crate::ParaAttrDelta::default();
    delta.alignment = crate::format::Patch::Set("center".to_owned());
    for (indexes, restyle) in [(vec![32], false), (vec![3, 40], false), (vec![3, 40], true)] {
        let selector = crate::ParaSelector::Many(indexes.iter().map(|&i| ids[i].clone()).collect());
        for ctx in [suggesting.clone(), EditCtx::local("", "")] {
            let expected = if restyle || ctx.is_suggesting() {
                selected_chunks(&indexes)
            } else {
                0
            };
            crate::SNAPSHOT_DIFFS_BUILT.set(0);
            crate::SNAPSHOT_CHUNKS_BUILT.set(0);
            let receipt = if restyle {
                doc.restyle_paragraphs(&ctx, &selector, "Heading1", &styles)
            } else {
                doc.set_paragraph_attrs(&ctx, &selector, &delta)
            }
            .unwrap();
            assert_eq!(crate::SNAPSHOT_CHUNKS_BUILT.get(), expected);
            assert_eq!(crate::SNAPSHOT_DIFFS_BUILT.get(), usize::from(expected > 0));
            if ctx.is_suggesting() && !restyle {
                assert_eq!(receipt.revision_ids.len(), 1);
                assert_eq!(receipt.revision_ids == inserted, indexes.contains(&40));
            }
        }
    }
}

/// The cached table embeds of `story_id` match a fresh read, and the lookups built on them agree.
fn assert_table_embeds(doc: &EditingDoc, story_id: &str) {
    let (cached, fresh) = {
        let txn = doc.yrs_doc().transact();
        let story = crate::story_ref(&txn, story_id).unwrap();
        let cached = doc.table_embeds(story_id, &story, &txn);
        let fresh = table::table_embeds_in(&story, &txn);
        let payloads = |embeds: &[(u32, yrs::MapRef)]| {
            embeds
                .iter()
                .map(|(offset, map)| (*offset, crate::embed_payload(map, &txn)))
                .collect::<Vec<_>>()
        };
        (payloads(&cached), payloads(&fresh))
    };
    assert_eq!(cached, fresh);
    for (ordinal, (offset, payload)) in fresh.iter().enumerate() {
        let locator = TableLocator::new(story_id, ordinal as u32);
        assert_eq!(doc.table_embed_index(&locator).unwrap(), *offset);
        assert_eq!(
            doc.table_locator_at_index(story_id, *offset).unwrap(),
            locator
        );
        assert_eq!(&doc.table_payload(&locator).unwrap(), payload);
    }
    assert!(
        doc.table_payload(&TableLocator::new(story_id, fresh.len() as u32))
            .is_err()
    );
}

#[test]
fn table_embeds_follow_plain_edits_and_reread_after_other_changes() {
    let doc = EditingDoc::new(9);
    let ctx = EditCtx::local("", "");
    doc.create_story("body", "Alpha", "Normal", "left").unwrap();
    for _ in 0..3 {
        let end = doc.story_len("body").unwrap();
        doc.insert_table(&ctx, Position::new("body", end), 1, 2)
            .unwrap();
    }
    assert_table_embeds(&doc, "body");
    let cell = TableLocator::new("body", 1);
    let cell_story = {
        let payload = doc.table_payload(&cell).unwrap();
        let Some(Any::Array(rows)) = payload.get("rows") else {
            panic!("rows")
        };
        let Any::Map(row) = &rows[0] else {
            panic!("row")
        };
        let Some(Any::Array(cells)) = row.get("cells") else {
            panic!("cells")
        };
        let Any::Map(first) = &cells[0] else {
            panic!("cell")
        };
        let Some(Any::String(story)) = first.get("story") else {
            panic!("story")
        };
        story.to_string()
    };
    crate::TABLE_EMBED_SCANS.set(0);
    for step in 0..8u32 {
        // plain typing and deleting before the tables carries the cache
        doc.paragraph_index("body").unwrap();
        let before = doc.committed_epoch();
        doc.insert_text(&ctx, Position::new("body", 1), "xy", FormatPolicy::Inherit)
            .unwrap();
        doc.advance_indexes_after_text_insert("body", before, doc.committed_epoch(), 1, "xy");
        assert_table_embeds(&doc, "body");
        let before = doc.committed_epoch();
        doc.delete_range(&ctx, StoryRange::new("body", 1 + step % 2, 2 + step % 2))
            .unwrap();
        doc.advance_indexes_after_text_delete(
            "body",
            before,
            doc.committed_epoch(),
            1 + step % 2,
            2 + step % 2,
        );
        assert_table_embeds(&doc, "body");
        // typing in a cell story leaves the body's tables where they are
        let before = doc.committed_epoch();
        doc.insert_text(
            &ctx,
            Position::new(cell_story.as_str(), 0),
            "c",
            FormatPolicy::Inherit,
        )
        .unwrap();
        doc.advance_indexes_after_text_insert(&cell_story, before, doc.committed_epoch(), 0, "c");
        assert_table_embeds(&doc, "body");
    }
    assert_eq!(
        crate::TABLE_EMBED_SCANS.get(),
        0,
        "plain edits re-read the body's tables"
    );

    // a deletion the paragraph index cannot place drops the story's tables; they are read again
    let before = doc.committed_epoch();
    doc.delete_range(&ctx, StoryRange::new("body", 1, 2))
        .unwrap();
    doc.advance_indexes_after_text_delete("body", before, doc.committed_epoch(), 1, 2);
    assert_table_embeds(&doc, "body");
    assert_eq!(crate::TABLE_EMBED_SCANS.get(), 1);

    // deleting a table, inserting one, and a remote merge each move to a new epoch unadvanced
    let first = doc
        .table_embed_index(&TableLocator::new("body", 0))
        .unwrap();
    doc.delete_range(&ctx, StoryRange::new("body", first, first + 1))
        .unwrap();
    assert_table_embeds(&doc, "body");
    doc.insert_table(&ctx, Position::new("body", 0), 2, 1)
        .unwrap();
    assert_table_embeds(&doc, "body");
    let peer = EditingDoc::new(10);
    peer.apply_update_v1(&doc.encode_state_as_update_v1())
        .unwrap();
    let end = peer.story_len("body").unwrap();
    peer.insert_table(&ctx, Position::new("body", end), 1, 1)
        .unwrap();
    peer.insert_text(
        &ctx,
        Position::new("body", 0),
        "remote",
        FormatPolicy::Inherit,
    )
    .unwrap();
    doc.apply_update_v1(&peer.encode_state_as_update_v1())
        .unwrap();
    assert_table_embeds(&doc, "body");
}
