# Vault 数据组件

`pos-vault` 提供可供桌面应用及其他模块复用的只读 Rust API。`pos` 将它暴露为 JSON 命令。所有命令都要显式指定 Vault 根目录。

```powershell
cargo run -p pos -- vault --root 'C:\path\to\vault' info
cargo run -p pos -- vault --root 'C:\path\to\vault' scan
cargo run -p pos -- vault --root 'C:\path\to\vault' list
cargo run -p pos -- vault --root 'C:\path\to\vault' read 'Library/Notes/示例.md'
cargo run -p pos -- vault --root 'C:\path\to\vault' validate
cargo run -p pos -- vault --root 'C:\path\to\vault' stats
cargo run -p pos -- vault --root 'C:\path\to\vault' scan --watch
```

`read` 返回 YAML 属性、Markdown 正文和双链信息。`scan` 返回统计和读取问题。`list` 返回笔记元数据。`validate` 额外报告重复 ID、找不到目标的双链和有多个候选的双链。校验定位到文件，保留标题和块锚点供调用方使用。

扫描遵守根目录的 `.obsidianignore`，跳过隐藏路径及符号链接。附件和 `AGENTS.md` 可以作为链接目标，扫描只读取其他 Markdown 文件的正文。扫描结果存于进程内 SQLite，`scan --watch` 先输出一份 JSON 报告，再在本地文件变化后重扫并逐行输出。

已有 YAML `id` 的笔记使用显式 ID。其余笔记用库内路径作为 ID，重命名或移动会改变这个 ID。组件不会向笔记写入 ID、索引或其他数据。
