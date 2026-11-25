use pos_vault::{IssueKind, Vault, VaultError};
use std::{fs, path::Path};

fn put(root: &Path, relative: &str, content: impl AsRef<[u8]>) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn reads_obsidian_metadata_without_rewriting_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = concat!(
        "\u{feff}---\r\n",
        "id: alpha\r\n",
        "tags: ['#chemistry', chemistry, permanent]\r\n",
        "aliases: [旧名称]\r\n",
        "extra: {count: 2}\r\n",
        "---\r\n",
        "[[笔记#标题|别名]] ![[图.png|300]] [[#^block]]\r\n",
        "| [[表格\\|文字]] |\r\n",
        "`[[inline]] %%` %% [[comment]] %% \\[[escaped]]\r\n",
        "```md\r\n[[fenced]]\r\n```\r\n",
    );
    put(temp.path(), "Library/原始笔记.md", source);

    let vault = Vault::open(temp.path()).unwrap();
    let note = vault.read("Library/原始笔记.md").unwrap();
    assert_eq!(note.metadata.title, "原始笔记");
    assert_eq!(note.metadata.id, "yaml:alpha");
    assert_eq!(note.metadata.tags, ["chemistry", "permanent"]);
    assert_eq!(note.metadata.aliases, ["旧名称"]);
    assert_eq!(note.frontmatter["extra"]["count"], 2);
    let links = note.metadata.links;
    assert_eq!(
        links
            .iter()
            .map(|link| link.target.as_str())
            .collect::<Vec<_>>(),
        ["笔记", "图.png", "", "表格"]
    );
    assert_eq!(links[0].fragment.as_deref(), Some("标题"));
    assert_eq!(links[0].label.as_deref(), Some("别名"));
    assert!(links[1].embed);
    assert_eq!(links[2].fragment.as_deref(), Some("^block"));
    assert_eq!(links[3].label.as_deref(), Some("文字"));
    assert!(note.body.starts_with("[[笔记"));
    assert_eq!(
        fs::read(temp.path().join("Library/原始笔记.md")).unwrap(),
        source.as_bytes()
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn reads_a_leading_thematic_break_as_markdown() {
    let temp = tempfile::tempdir().unwrap();
    let source = "---\nA paragraph after a thematic break.\n\nExample: [[Related]]\n";
    put(temp.path(), "Note.md", source);
    let vault = Vault::open(temp.path()).unwrap();
    let note = vault.read("Note.md").unwrap();
    assert_eq!(note.body, source);
    assert!(note.frontmatter.is_empty());
    assert_eq!(note.metadata.links[0].target, "Related");
}

#[test]
fn indexes_included_files_and_reports_distinct_validation_problems() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    put(root, ".obsidianignore", "Skills/\n");
    put(root, ".obsidian/app.json", "{}");
    put(root, "Skills/hidden.md", "");
    put(root, "Library/.private/hidden.md", "");
    put(root, "AGENTS.md", "");
    put(root, "Attachments/tool/README.md", "---\ninvalid: [\n---\n");
    put(root, "Attachments/图.png", []);
    put(
        root,
        "Library/Source.md",
        concat!(
            "---\nid: 42\n---\n",
            "[[Existing]] [[./Existing]] [[Library/Existing]] [[Library/Existing.md]] ",
            "[[../Research/Ambiguous]] [[/Research/Ambiguous]] ![[图.png]] ",
            "[[Ambiguous]] [[Missing]] [[#标题]]",
        ),
    );
    put(root, "Library/Existing.md", "---\nid: '42'\n---\n");
    put(root, "Research/Ambiguous.md", "");
    put(root, "Journal/Ambiguous.md", "");
    put(root, "Library/Invalid.md", "---\ntags: [\n---\n");
    put(root, "Library/InvalidUtf8.md", [0xff]);

    let vault = Vault::open(root).unwrap();
    assert!(vault.info().obsidian_config);
    for path in [
        "../outside.md",
        "Skills/hidden.md",
        ".obsidian/app.json",
        "Attachments/tool/README.md",
        "AGENTS.md",
    ] {
        assert!(
            matches!(vault.read(path), Err(VaultError::InvalidNotePath(_))),
            "{path}"
        );
    }
    assert!(vault.read(root.join("Library/Source.md")).is_err());
    let index = vault.scan().unwrap();
    let stats = index.stats().unwrap();
    assert_eq!(stats.notes, 4);
    assert_eq!(stats.scan_issues, 2);
    assert_eq!(stats.links, 10);
    let issues = index.validate().unwrap();
    assert_eq!(
        issues
            .iter()
            .filter(|issue| issue.kind == IssueKind::DuplicateId)
            .count(),
        2
    );
    assert_eq!(
        issues
            .iter()
            .filter(|issue| issue.kind == IssueKind::AmbiguousLink)
            .count(),
        1
    );
    assert_eq!(
        issues
            .iter()
            .filter(|issue| issue.kind == IssueKind::UnresolvedLink)
            .count(),
        1
    );
    assert_eq!(issues.len(), 6);
    assert_eq!(
        vault.read("Journal/Ambiguous.md").unwrap().metadata.id,
        "path:Journal/Ambiguous.md"
    );
    assert_eq!(
        vault.read("Library/Source.md").unwrap().metadata.id,
        "yaml:42"
    );
    assert_eq!(fs::read_dir(root).unwrap().count(), 8);
}

#[cfg(unix)]
#[test]
fn skips_links_outside_the_vault() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    put(outside.path(), "outside.md", "outside");
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("outside.md"),
        root.path().join("link.md"),
    )
    .unwrap();
    let vault = Vault::open(root.path()).unwrap();
    assert!(vault.read("linked/outside.md").is_err());
    assert!(vault.read("link.md").is_err());
    assert_eq!(vault.scan().unwrap().stats().unwrap().files, 0);
}
