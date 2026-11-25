# Vault 数据组件

`pos-vault` 提供可供桌面应用及其他模块复用的只读 Rust API。`pos` 将它暴露为 JSON 命令。所有命令都要显式指定 Vault 根目录。

```powershell
cargo run -p pos -- vault --root 'C:\path\to\vault' info
cargo run -p pos -- vault --root 'C:\path\to\vault' scan
cargo run -p pos -- vault --root 'C:\path\to\vault' list
cargo run -p pos -- vault --root 'C:\path\to\vault' read 'Library/Notes/示例.md'
cargo run -p pos -- vault --root 'C:\path\to\vault' validate
cargo run -p pos -- vault --root 'C:\path\to\vault' resolve 'Source.md' '示例#标题'
cargo run -p pos -- vault --root 'C:\path\to\vault' backlinks 'Library/Notes/示例.md'
cargo run -p pos -- vault --root 'C:\path\to\vault' stats
cargo run -p pos -- vault --root 'C:\path\to\vault' scan --watch
```

`read` 返回 YAML 属性、Markdown 正文、双链、标题和块。标题与块范围按原文件的 UTF-8 字节计数，包含 BOM。Rust API 可读取原文件并获取 WikiLink 的字节范围。`resolve` 校验文件、标题和块锚点，`backlinks` 查找指向实际目标的链接。`validate` 汇报不存在的目标、缺失锚点、歧义和重复 ID。

扫描遵守根目录的 `.obsidianignore`，跳过隐藏路径及符号链接。附件和 `AGENTS.md` 可以作为链接目标，扫描只读取其他 Markdown 文件的正文。Obsidian Markdown 是源数据，SQLite 索引保存在进程内。`scan --watch` 先输出初始报告，再增量更新已收集的文件路径。目录、忽略规则和通知溢出触发重扫。

已有 YAML `id` 的笔记使用显式 ID。其他笔记以文件系统身份识别，同卷重命名可保留 ID。版本库记录原子替换和备份恢复产生的身份映射。启用外部观察后，同路径的原子保存会通过已有基线核对文件身份。Vault 的读取和索引均保持只读。

`prepare_property` 生成并验证顶层 YAML 属性的增删改补丁，保留其他属性和正文的字节，由 Change 统一审批和执行。
