use crate::filesystem::Layout;
use crate::store::Store;
use crate::{
    ChangeEngine, ChangeError, ChangePlan, ChangeRequest, ExclusiveWrite, Operation, RecoveryStatus,
};
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

fn create(source: &str) -> (TempDir, ChangeEngine) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("Note.md"), source).unwrap();
    let engine = ChangeEngine::open(directory.path()).unwrap();
    (directory, engine)
}

fn preview(engine: &ChangeEngine, operation: Operation) -> ChangePlan {
    engine.preview(request(operation)).unwrap()
}

fn request(operation: Operation) -> ChangeRequest {
    ChangeRequest {
        change_id: Uuid::new_v4(),
        path: "Note.md".into(),
        actor_id: "fixture".into(),
        device_id: None,
        reason: Some("Verify semantic edits".into()),
        operation,
    }
}

fn note(root: &Path) -> String {
    fs::read_to_string(root.join("Note.md")).unwrap()
}

#[test]
fn preview_is_read_only_and_undo_preserves_an_independent_link() {
    let original =
        "\u{feff}---\r\nid: demo\r\n---\r\n第一段：配位平衡。\r\n\r\n第二段：副反应。\r\n";
    let (temp, engine) = create(original);
    let unbound = preview(&engine, Operation::link_add("配位平衡", "配位平衡", None));
    assert!(unbound.vault_id.is_none());
    assert!(engine.list().unwrap().is_empty());
    assert!(!temp.path().join(".maggot").exists());
    assert_eq!(note(temp.path()), original);
    let store = engine.initialize().unwrap();
    assert_eq!(engine.initialize().unwrap().vault_id, store.vault_id);

    let exclusive = ExclusiveWrite::confirmed();
    let first = preview(&engine, Operation::link_add("配位平衡", "配位平衡", None));
    let first_record = engine.apply(&first, &exclusive).unwrap();
    assert_eq!(
        engine.apply(&first, &exclusive).unwrap().seq,
        first_record.seq
    );
    let second = preview(
        &engine,
        Operation::link_add("副反应", "Library/副反应", None),
    );
    assert!(second.depends_on.is_empty());
    engine.apply(&second, &exclusive).unwrap();
    let undo = engine
        .preview_revert(first.request.change_id, "fixture".into(), None, None)
        .unwrap();
    engine.apply(&undo, &exclusive).unwrap();
    assert_eq!(
        note(temp.path()),
        original.replace("副反应", "[[Library/副反应|副反应]]")
    );
    assert_eq!(engine.list().unwrap().len(), 3);
    assert_eq!(
        engine.show(undo.request.change_id).unwrap().plan.reverts,
        Some(first.request.change_id)
    );
    assert!(
        engine
            .diff(first.request.change_id)
            .unwrap()
            .contains("+第一段：[[配位平衡]]。")
    );
    assert!(
        engine
            .preview_revert(first.request.change_id, "fixture".into(), None, None)
            .is_err()
    );
}

#[test]
fn a_dependent_change_must_be_reverted_first() {
    let (temp, engine) = create("An example: term.\n");
    engine.initialize().unwrap();
    let exclusive = ExclusiveWrite::confirmed();
    let first = preview(&engine, Operation::link_add("term", "Canonical", None));
    engine.apply(&first, &exclusive).unwrap();
    let second = preview(&engine, Operation::link_remove("[[Canonical|term]]", None));
    assert_eq!(second.depends_on, [first.request.change_id]);
    engine.apply(&second, &exclusive).unwrap();
    assert!(matches!(
        engine.preview_revert(first.request.change_id, "fixture".into(), None, None),
        Err(ChangeError::Conflict(message)) if message.contains("still depends")
    ));
    let restore = engine
        .preview_revert(second.request.change_id, "fixture".into(), None, None)
        .unwrap();
    engine.apply(&restore, &exclusive).unwrap();
    let undo = engine
        .preview_revert(first.request.change_id, "fixture".into(), None, None)
        .unwrap();
    engine.apply(&undo, &exclusive).unwrap();
    assert_eq!(note(temp.path()), "An example: term.\n");
}

#[test]
fn preview_ignores_non_text_and_rejects_tampering_or_stale_files() {
    let original = "---\nexample: word\n---\n`word` %% word %% [word](https://example.com)\nPlain word here.\n";
    let (temp, engine) = create(original);
    engine.initialize().unwrap();
    let approved = preview(&engine, Operation::link_add("word", "word", None));
    assert_eq!(approved.patch.start, original.rfind("word").unwrap());
    let mut tampered = approved.clone();
    tampered.patch.after = "[[different]]".into();
    let exclusive = ExclusiveWrite::confirmed();
    assert!(matches!(
        engine.apply(&tampered, &exclusive),
        Err(ChangeError::Conflict(_))
    ));
    fs::write(
        temp.path().join("Note.md"),
        original.to_owned() + "\nOutside edit.\n",
    )
    .unwrap();
    assert!(matches!(
        engine.apply(&approved, &exclusive),
        Err(ChangeError::Conflict(_))
    ));
    assert!(note(temp.path()).ends_with("Outside edit.\n"));
    assert!(engine.list().unwrap().is_empty());
    assert!(engine.recover().unwrap().is_empty());
}

#[test]
fn overlapping_occurrences_require_an_explicit_offset() {
    let (_temp, engine) = create("banana\n");
    let ambiguous = engine.preview(request(Operation::link_add("ana", "ana", None)));
    assert!(
        matches!(ambiguous, Err(ChangeError::Conflict(message)) if message.contains("multiple"))
    );
    let selected = preview(&engine, Operation::link_add("ana", "ana", Some(3)));
    assert_eq!(selected.patch.start, 3);
}

#[test]
fn recovers_before_replace_and_requires_evidence_after_replace() {
    let original = "Example term.\n";
    let (temp, engine) = create(original);
    engine.initialize().unwrap();
    let exclusive = ExclusiveWrite::confirmed();
    let before_replace = preview(&engine, Operation::link_add("term", "term", None));
    let after_replace = preview(&engine, Operation::link_add("term", "term", None));
    let changed = "Example [[term]].\n";
    let layout = Layout::new(&before_replace.vault_root);
    let stage = |plan: &ChangePlan, replace: bool| {
        let _lock = layout.lock().unwrap();
        let mut store = Store::open(&layout, true).unwrap().unwrap();
        store
            .prepare(plan, original.as_bytes(), changed.as_bytes())
            .unwrap();
        if replace {
            layout
                .replace(
                    plan.request.change_id,
                    &plan.vault_root.join("Note.md"),
                    &plan.before_hash,
                    changed.as_bytes(),
                )
                .unwrap();
        }
    };
    stage(&before_replace, false);
    assert_eq!(
        engine.recover().unwrap()[0].status,
        RecoveryStatus::NotApplied
    );
    assert_eq!(
        engine
            .resolve(before_replace.request.change_id, false, &exclusive)
            .unwrap()
            .status,
        RecoveryStatus::NotApplied
    );
    assert_eq!(note(temp.path()), original);
    stage(&after_replace, true);
    assert_eq!(note(temp.path()), changed);
    assert_eq!(
        engine.recover().unwrap()[0].status,
        RecoveryStatus::AwaitingConfirmation
    );
    assert!(engine.list().unwrap().is_empty());
    assert_eq!(
        engine
            .resolve(after_replace.request.change_id, true, &exclusive)
            .unwrap()
            .status,
        RecoveryStatus::Recovered
    );
    assert_eq!(engine.apply(&after_replace, &exclusive).unwrap().seq, 1);
    assert!(engine.recover().unwrap().is_empty());
}

#[test]
fn undo_rejects_a_copied_link_after_an_external_edit() {
    let (temp, engine) = create("Start\n\nA term Z.\n");
    engine.initialize().unwrap();
    let exclusive = ExclusiveWrite::confirmed();
    let change = preview(&engine, Operation::link_add("term", "term", None));
    engine.apply(&change, &exclusive).unwrap();
    let outside = "Start\n\nB [[term]] Z.\n\nA [[term]] Z.\n";
    fs::write(temp.path().join("Note.md"), outside).unwrap();
    assert!(matches!(
        engine.preview_revert(change.request.change_id, "fixture".into(), None, None),
        Err(ChangeError::Conflict(_))
    ));
    assert_eq!(note(temp.path()), outside);
    assert_eq!(engine.list().unwrap().len(), 1);
}

#[test]
fn changes_reject_duplicate_declared_identity() {
    let (temp, engine) = create("---\nid: collision\n---\nUse term.\n");
    fs::write(
        temp.path().join("Other.md"),
        "---\nid: collision\n---\nOther note.\n",
    )
    .unwrap();
    engine.initialize().unwrap();
    let result = engine.preview(request(Operation::link_add("term", "term", None)));
    assert!(matches!(
        result,
        Err(ChangeError::Vault(
            pos_vault::VaultError::InvalidNote { .. }
        ))
    ));
    assert!(engine.list().unwrap().is_empty());
}

#[test]
fn keep_current_closes_conflict_and_preserves_external_bytes() {
    let (temp, engine) = create("Original term.\n");
    engine.initialize().unwrap();
    let exclusive = ExclusiveWrite::confirmed();
    let change = preview(&engine, Operation::link_add("term", "term", None));
    let layout = Layout::new(&change.vault_root);
    {
        let _lock = layout.lock().unwrap();
        let mut store = Store::open(&layout, true).unwrap().unwrap();
        store
            .prepare(&change, b"Original term.\n", b"Original [[term]].\n")
            .unwrap();
    }
    let outside = "Direct edit to term.\n";
    fs::write(temp.path().join("Note.md"), outside).unwrap();
    assert_eq!(
        engine.recover().unwrap()[0].status,
        RecoveryStatus::Conflict
    );
    let kept = engine
        .keep_current(change.request.change_id, &exclusive)
        .unwrap();
    assert_eq!(kept.status, RecoveryStatus::ClosedConflict);
    assert_eq!(note(temp.path()), outside);
    assert!(engine.recover().unwrap().is_empty());
    let store = Store::open(&layout, false).unwrap().unwrap();
    assert_eq!(
        store.blob(kept.current_hash.as_deref().unwrap()).unwrap(),
        outside
    );
    drop(store);
    let next = preview(&engine, Operation::link_add("term", "term", None));
    engine.apply(&next, &exclusive).unwrap();
    assert_eq!(note(temp.path()), "Direct edit to [[term]].\n");
}

#[test]
fn can_create_insert_and_edit_properties_without_reformatting() {
    let (temp, engine) = create("Blank fixture\n");
    engine.initialize().unwrap();
    let exclusive = ExclusiveWrite::confirmed();
    let original =
        "\u{feff}---\r\nstatus: draft # keep\r\n# other\r\nlabels: [team]\r\n---\r\n# Title\r\n";
    let created = engine
        .preview(ChangeRequest {
            path: "Schedule.md".into(),
            ..request(Operation::create_note(original))
        })
        .unwrap();
    engine.apply(&created, &exclusive).unwrap();
    let saved = pos_vault::Vault::open(temp.path()).unwrap();
    assert_eq!(
        saved.read_source("Schedule.md").unwrap().metadata.id,
        created.note_id
    );
    let set = engine
        .preview(ChangeRequest {
            path: "Schedule.md".into(),
            ..request(Operation::set_property(
                "status",
                serde_json::json!("ready"),
            ))
        })
        .unwrap();
    engine.apply(&set, &exclusive).unwrap();
    let status_version = original.replace("status: draft", "status: \"ready\"");
    assert_eq!(
        fs::read_to_string(temp.path().join("Schedule.md")).unwrap(),
        status_version
    );
    let insertion = engine
        .preview(ChangeRequest {
            path: "Schedule.md".into(),
            ..request(Operation::replace_markdown(
                status_version.len(),
                status_version.len(),
                "Item\r\n",
            ))
        })
        .unwrap();
    engine.apply(&insertion, &exclusive).unwrap();
    let undo_status = engine
        .preview_revert(set.request.change_id, "fixture".into(), None, None)
        .unwrap();
    engine.apply(&undo_status, &exclusive).unwrap();
    assert_eq!(
        fs::read_to_string(temp.path().join("Schedule.md")).unwrap(),
        format!("{original}Item\r\n")
    );
    let remove = engine
        .preview(ChangeRequest {
            path: "Schedule.md".into(),
            ..request(Operation::remove_property("status"))
        })
        .unwrap();
    engine.apply(&remove, &exclusive).unwrap();
    let text = fs::read_to_string(temp.path().join("Schedule.md")).unwrap();
    assert!(text.contains("# keep\r\n# other\r\nlabels: [team]"));
    assert!(
        !saved
            .read("Schedule.md")
            .unwrap()
            .frontmatter
            .contains_key("status")
    );
    assert!(
        engine
            .preview_revert(created.request.change_id, "fixture".into(), None, None)
            .is_err()
    );
    let empty = engine
        .preview(ChangeRequest {
            path: "Empty.md".into(),
            ..request(Operation::create_note(""))
        })
        .unwrap();
    engine.apply(&empty, &exclusive).unwrap();
    let undo_empty = engine
        .preview_revert(empty.request.change_id, "fixture".into(), None, None)
        .unwrap();
    engine.apply(&undo_empty, &exclusive).unwrap();
    assert!(!temp.path().join("Empty.md").exists());
}

#[test]
fn records_external_edits_renames_and_deletions() {
    let (temp, engine) = create("Original.\n");
    engine.initialize().unwrap();
    assert_eq!(engine.observe("external").unwrap().baselines, 1);
    fs::write(temp.path().join("Note.md"), "Edited.\n").unwrap();
    let edit = engine.observe("external").unwrap().changes.pop().unwrap();
    assert_eq!(edit.plan.patch.before, "Original.\n");
    assert_eq!(edit.plan.patch.after, "Edited.\n");
    fs::rename(temp.path().join("Note.md"), temp.path().join("Renamed.md")).unwrap();
    let renamed = engine.observe("external").unwrap().changes.pop().unwrap();
    assert_eq!(renamed.plan.note_id, edit.plan.note_id);
    assert_eq!(
        renamed.plan.request.operation.payload["previous_path"],
        "Note.md"
    );
    assert_eq!(renamed.plan.before_hash, renamed.plan.after_hash);
    fs::remove_file(temp.path().join("Renamed.md")).unwrap();
    let deleted = engine.observe("external").unwrap().changes.pop().unwrap();
    assert_eq!(deleted.plan.note_id, edit.plan.note_id);
    assert!(!deleted.plan.after_exists);
    assert!(engine.observe("external").unwrap().changes.is_empty());
}

#[test]
fn independent_backup_restores_identity_bytes_and_revert_history() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("vault");
    let archive = parent.path().join("archive");
    let restored = parent.path().join("restored");
    fs::create_dir_all(root.join("Attachments")).unwrap();
    fs::write(root.join("Attachments/data.bin"), [1, 0, 2]).unwrap();
    fs::write(root.join("Note.md"), "Original term.\n").unwrap();
    let engine = ChangeEngine::open(&root).unwrap();
    engine.initialize().unwrap();
    let exclusive = ExclusiveWrite::confirmed();
    let change = preview(&engine, Operation::link_add("term", "term", None));
    engine.apply(&change, &exclusive).unwrap();
    fs::rename(root.join("Note.md"), root.join("Renamed.md")).unwrap();
    let rename = engine.observe("external").unwrap().changes.pop().unwrap();
    assert_eq!(rename.plan.note_id, change.note_id);
    let snapshot = crate::create_backup(&root, &archive, &exclusive).unwrap();
    assert_eq!(snapshot.last_sequence, Some(2));
    crate::verify_backup(&archive).unwrap();
    fs::write(root.join("Renamed.md"), "After backup\n").unwrap();
    crate::restore_backup(&archive, &restored).unwrap();
    let restored_vault = pos_vault::Vault::open(&restored).unwrap();
    assert_eq!(
        restored_vault
            .read_source("Renamed.md")
            .unwrap()
            .metadata
            .id,
        change.note_id
    );
    assert_eq!(
        fs::read(restored.join("Attachments/data.bin")).unwrap(),
        [1, 0, 2]
    );
    let restored_engine = ChangeEngine::open(&restored).unwrap();
    assert_eq!(restored_engine.list().unwrap().len(), 2);
    let undo = restored_engine
        .preview_revert(change.request.change_id, "fixture".into(), None, None)
        .unwrap();
    restored_engine.apply(&undo, &exclusive).unwrap();
    assert_eq!(
        fs::read_to_string(restored.join("Renamed.md")).unwrap(),
        "Original term.\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("Renamed.md")).unwrap(),
        "After backup\n"
    );
    fs::write(archive.join("vault/Renamed.md"), "Tampered\n").unwrap();
    assert!(crate::verify_backup(&archive).is_err());
}

#[test]
fn v1_store_is_readable_and_migrates_on_explicit_write() {
    let (temp, engine) = create("Original term.\n");
    engine.initialize().unwrap();
    let change = preview(&engine, Operation::link_add("term", "term", None));
    engine.apply(&change, &ExclusiveWrite::confirmed()).unwrap();
    let path = temp.path().join(".maggot/changes.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE execution_conflicts; DROP TABLE note_bindings; DROP TABLE observed_notes; DROP TABLE observation_state; PRAGMA user_version = 1;").unwrap();
    drop(db);
    assert_eq!(engine.list().unwrap().len(), 1);
    assert_eq!(
        engine.initialize().unwrap().vault_id,
        change.vault_id.unwrap()
    );
    let db = rusqlite::Connection::open(path).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(engine.list().unwrap().len(), 1);
}
