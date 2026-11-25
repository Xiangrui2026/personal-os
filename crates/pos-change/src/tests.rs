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
