//! Nerd Font and Unicode emoji file icons with a fixed two-column slot.
//! Glyphs: https://github.com/ryanoasis/nerd-fonts/blob/master/glyphnames.json
//! Nerd mode needs a patched terminal font; Unicode mode uses two-column emoji.
use super::*;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(in crate::tui) enum IconMode {
    #[default]
    Nerd,
    Unicode,
}
impl IconMode {
    pub(in crate::tui) fn toggle(&mut self) {
        *self = match self {
            Self::Nerd => Self::Unicode,
            Self::Unicode => Self::Nerd,
        };
    }
    pub(in crate::tui) fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    pub(in crate::tui) fn save(self, path: &Path) -> Result<()> {
        crate::codex::atomic_write(path, &serde_json::to_vec(&self)?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileIcon {
    pub(super) glyph: &'static str,
    pub(super) color: Color,
}
const fn icon(glyph: &'static str, color: Color) -> FileIcon {
    FileIcon { glyph, color }
}

fn nerd_icon(path: &Path, directory: bool, link: bool) -> FileIcon {
    if link {
        return icon("\u{f0c1}", BLUE);
    }
    if directory {
        return icon("\u{f07b}", BLUE);
    }
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if name.starts_with("readme") || name.starts_with("changelog") {
        return icon("\u{f15c}", INK);
    }
    if name.starts_with("license") || name.starts_with("licence") || name.starts_with("copying") {
        return icon("\u{e60a}", GOLD);
    }
    if name == ".git" || name.starts_with(".git") {
        return icon("\u{e702}", RED);
    }
    if name == "dockerfile" || name.starts_with("dockerfile.") || name == "containerfile" {
        return icon("\u{e650}", BLUE);
    }
    if matches!(
        name.as_str(),
        "makefile" | "gnumakefile" | "justfile" | "taskfile"
    ) {
        return icon("\u{e691}", GREEN);
    }
    if name == ".env"
        || name.starts_with(".env.")
        || name.ends_with("rc")
        || matches!(
            name.as_str(),
            "cargo.lock"
                | "package-lock.json"
                | "yarn.lock"
                | "pnpm-lock.yaml"
                | "bun.lock"
                | "bun.lockb"
        )
    {
        return icon("\u{e615}", GOLD);
    }
    match extension.as_str() {
        "rs" => icon("\u{e68b}", RED),
        "ts" | "tsx" | "mts" | "cts" => icon("\u{e628}", BLUE),
        "js" | "jsx" | "mjs" | "cjs" => icon("\u{e60c}", GOLD),
        "py" | "pyi" | "pyw" => icon("\u{e606}", GREEN),
        "go" => icon("\u{e627}", BLUE),
        "c" | "h" => icon("\u{e649}", BLUE),
        "cpp" | "hpp" | "cc" | "cxx" | "hxx" => icon("\u{e646}", BLUE),
        "cs" => icon("\u{e648}", GREEN),
        "java" => icon("\u{e66d}", RED),
        "kt" | "kts" => icon("\u{e634}", RED),
        "rb" => icon("\u{e605}", RED),
        "swift" => icon("\u{e699}", RED),
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => icon("\u{e691}", GREEN),
        "html" | "htm" | "xml" => icon("\u{e60e}", RED),
        "vue" => icon("\u{e6a0}", GREEN),
        "svelte" => icon("\u{e697}", RED),
        "css" | "scss" | "sass" | "less" => icon("\u{e614}", BLUE),
        "json" | "jsonc" | "json5" | "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg"
        | "properties" | "lock" => icon("\u{e615}", GOLD),
        "md" | "mdx" => icon("\u{e609}", INK),
        "pdf" => icon("\u{f1c1}", RED),
        "doc" | "docx" => icon("\u{f1c2}", BLUE),
        "txt" | "rst" | "adoc" | "rtf" => icon("\u{f15c}", INK),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp" | "avif" | "heic"
        | "tif" | "tiff" => icon("\u{f1c5}", GREEN),
        "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a" | "aiff" => icon("\u{f1c7}", GREEN),
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => icon("\u{f1c8}", BLUE),
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "tgz" => {
            icon("\u{f1c6}", GOLD)
        }
        "csv" | "tsv" | "db" | "sqlite" | "sqlite3" | "parquet" | "xlsx" | "xls" => {
            icon("\u{f1c0}", GREEN)
        }
        "sql" => icon("\u{f1c0}", GREEN),
        "exe" | "dll" | "so" | "dylib" | "bin" | "wasm" | "o" | "a" => icon("\u{f2db}", SOFT),
        "log" => icon("\u{f15c}", SOFT),
        _ => icon("\u{f016}", SOFT),
    }
}

pub(super) fn for_path(path: &Path, directory: bool, link: bool, mode: IconMode) -> FileIcon {
    let badge = nerd_icon(path, directory, link);
    if mode == IconMode::Nerd {
        return badge;
    }
    // Both modes share the same filename classification; emoji use two display columns.
    let glyph = match badge.glyph {
        "\u{f07b}" => "📁",
        "\u{f0c1}" => "🔗",
        "\u{e68b}" => "🦀",
        "\u{e628}" => "🟦",
        "\u{e60c}" => "🟨",
        "\u{e606}" => "🐍",
        "\u{e627}" => "🐹",
        "\u{e66d}" | "\u{e634}" => "☕",
        "\u{e605}" => "💎",
        "\u{e699}" => "🐦",
        "\u{e649}" | "\u{e646}" | "\u{e648}" => "💻",
        "\u{e691}" => "💻",
        "\u{e60e}" | "\u{e6a0}" | "\u{e697}" => "🌐",
        "\u{e614}" => "🎨",
        "\u{e615}" => "⚙️",
        "\u{e650}" => "🐳",
        "\u{e702}" => "🔀",
        "\u{e60a}" => "📜",
        "\u{e609}" => "📝",
        "\u{f1c1}" => "📕",
        "\u{f1c2}" => "📘",
        "\u{f1c5}" => "🖼️",
        "\u{f1c7}" => "🎵",
        "\u{f1c8}" => "🎬",
        "\u{f1c6}" => "📦",
        "\u{f1c0}" => "🗃️",
        "\u{f2db}" => "🔧",
        _ => "📄",
    };
    icon(glyph, badge.color)
}

pub(super) fn name_row(
    entry: &crate::files::Entry,
    name: &str,
    selected: bool,
    width: u16,
    mode: IconMode,
) -> Line<'static> {
    let badge = for_path(&entry.path, entry.directory, entry.link, mode);
    let style = Style::default()
        .fg(if selected {
            GOLD
        } else if entry.directory {
            BLUE
        } else {
            INK
        })
        .bg(if selected { RAIL } else { BG })
        .add_modifier(if selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });
    let marker = if selected { "▎ " } else { "  " };
    let glyph = format!(
        "{}{}",
        badge.glyph,
        " ".repeat(2usize.saturating_sub(badge.glyph.width()))
    );
    let label = format!("{name}{}", if entry.directory { "/" } else { "" });
    if width < 5 {
        return Line::styled(
            if width >= 4 {
                format!("{marker}{glyph}")
            } else {
                clipped(marker, width.into())
            },
            style,
        );
    }
    let label = clipped(&label, usize::from(width - 5));
    let padding = usize::from(width - 5).saturating_sub(label.width());
    Line::from(vec![
        Span::styled(marker, style),
        Span::styled(glyph, style.fg(badge.color)),
        Span::styled(format!(" {label}{}", " ".repeat(padding)), style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filenames_extensions_and_link_precedence() {
        for (name, directory, link, expected) in [
            ("src", true, false, "\u{f07b}"),
            ("link.rs", false, true, "\u{f0c1}"),
            ("dir", true, true, "\u{f0c1}"),
            ("main.rs", false, false, "\u{e68b}"),
            ("App.TSX", false, false, "\u{e628}"),
            ("script.py", false, false, "\u{e606}"),
            ("Cargo.toml", false, false, "\u{e615}"),
            (".env.local", false, false, "\u{e615}"),
            (".gitignore", false, false, "\u{e702}"),
            ("Dockerfile.dev", false, false, "\u{e650}"),
            ("README", false, false, "\u{f15c}"),
            ("LICENSE-MIT", false, false, "\u{e60a}"),
            ("照片.PNG", false, false, "\u{f1c5}"),
            ("music.flac", false, false, "\u{f1c7}"),
            ("video.mov", false, false, "\u{f1c8}"),
            ("bundle.tar.gz", false, false, "\u{f1c6}"),
            ("data.sqlite3", false, false, "\u{f1c0}"),
            ("new.unknown", false, false, "\u{f016}"),
        ] {
            let badge = for_path(Path::new(name), directory, link, IconMode::Nerd);
            assert_eq!(badge.glyph, expected, "{name}");
            assert_eq!(badge.glyph.chars().count(), 1);
            assert!((0xe000..=0xf8ff).contains(&(badge.glyph.chars().next().unwrap() as u32)));
            assert!(badge.glyph.width() <= 2);
        }
    }
    #[test]
    fn badge_column_aligns_names_and_retains_color_when_selected() {
        for (name, directory, link) in [
            ("main.rs", false, false),
            ("src", true, false),
            ("中文.txt", false, false),
            ("link", false, true),
        ] {
            let entry = crate::files::Entry {
                path: name.into(),
                directory,
                link,
                size: 0,
                modified: None,
            };
            for width in [0, 1, 4, 5, 12, 28, 44] {
                let row = name_row(&entry, name, true, width, IconMode::Nerd);
                assert!(row.width() <= usize::from(width));
                if width >= 5 {
                    assert_eq!(row.spans[0].width() + row.spans[1].width(), 4);
                    assert_eq!(
                        row.spans[1].style.fg,
                        Some(for_path(&entry.path, directory, link, IconMode::Nerd).color)
                    );
                    assert_eq!(row.width(), usize::from(width));
                    assert!(row.spans.iter().all(|span| span.style.bg == Some(RAIL)));
                }
            }
        }
    }
    #[test]
    fn emoji_icons_use_two_columns_and_align_filenames_at_all_sizes() {
        for (name, directory, link, glyph) in [
            ("src", true, false, "📁"),
            ("shortcut", true, true, "🔗"),
            ("main.rs", false, false, "🦀"),
            ("app.ts", false, false, "🟦"),
            ("config.toml", false, false, "⚙️"),
            ("image.png", false, false, "🖼️"),
            ("music.mp3", false, false, "🎵"),
            ("video.mp4", false, false, "🎬"),
            ("archive.zip", false, false, "📦"),
            ("data.db", false, false, "🗃️"),
            ("unknown", false, false, "📄"),
        ] {
            let entry = crate::files::Entry {
                path: name.into(),
                directory,
                link,
                size: 0,
                modified: None,
            };
            assert_eq!(
                for_path(&entry.path, directory, link, IconMode::Unicode).glyph,
                glyph
            );
            assert_eq!(glyph.width(), 2, "{name}");
            for width in [0, 1, 2, 3, 4, 5, 12, 28, 44] {
                for mode in [IconMode::Nerd, IconMode::Unicode] {
                    let row = name_row(&entry, name, true, width, mode);
                    assert!(row.width() <= usize::from(width), "{name}: width {width}");
                    if width >= 5 {
                        assert_eq!(row.spans[0].width() + row.spans[1].width(), 4);
                        assert_eq!(row.width(), usize::from(width));
                    }
                }
            }
        }
    }
    #[test]
    fn mode_save_load_and_missing_or_invalid_defaults() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mode.json");
        assert_eq!(IconMode::load(&path), IconMode::Nerd);
        IconMode::Unicode.save(&path).unwrap();
        assert_eq!(IconMode::load(&path), IconMode::Unicode);
        let mut mode = IconMode::Unicode;
        mode.toggle();
        assert_eq!(mode, IconMode::Nerd);
        mode.save(&path).unwrap();
        assert_eq!(IconMode::load(&path), IconMode::Nerd);
        std::fs::write(&path, "invalid").unwrap();
        assert_eq!(IconMode::load(&path), IconMode::Nerd);
    }
}
