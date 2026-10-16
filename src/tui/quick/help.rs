//! Compact, shared help layout for the Pulse sidebar.
use super::*;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum HelpLanguage {
    #[default]
    Chinese,
    English,
}

impl HelpLanguage {
    pub(super) fn text(self, chinese: &'static str, english: &'static str) -> &'static str {
        match self {
            Self::Chinese => chinese,
            Self::English => english,
        }
    }

    pub(super) fn toggle(&mut self) {
        *self = match self {
            Self::Chinese => Self::English,
            Self::English => Self::Chinese,
        };
    }

    pub(super) fn switch_label(self) -> &'static str {
        self.text("English(l)", "中文(l)")
    }
}

pub(super) fn help_footer(area: Rect, language: HelpLanguage) -> Vec<FooterButton> {
    layout_footer(
        area,
        vec![
            footer_control(
                [
                    language.switch_label(),
                    language.text("English", "中文"),
                    language.text("EN", "中"),
                ],
                KeyCode::Char('l'),
                true,
            ),
            footer_control(
                [
                    language.text("返回(q)", "Back(q)"),
                    language.text("返回", "Back"),
                    "q",
                ],
                KeyCode::Char('q'),
                true,
            ),
            footer_control(["Home(H)", "Home", "H"], KeyCode::Char('H'), true),
        ],
    )
}

pub(super) fn help_area(screen: Rect) -> (Rect, Rect) {
    let area = screen;
    let body = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width.saturating_sub(1),
        area.height.saturating_sub(3),
    );
    (area, body)
}

#[derive(Clone, Copy)]
pub(super) enum HelpPage {
    Usage,
    Sessions,
    Charts,
    Git,
    Log,
    File,
}

pub(super) fn help_content(
    page: HelpPage,
    language: HelpLanguage,
    width: u16,
) -> Vec<Line<'static>> {
    let mut out = vec![];
    let title = match page {
        HelpPage::Usage => language.text("用量帮助", "Token help"),
        HelpPage::Sessions => language.text("会话帮助", "Token help · Sessions"),
        HelpPage::Charts => language.text("图表帮助", "Token help · Charts"),
        HelpPage::Git => language.text("Git 帮助", "Git help"),
        HelpPage::Log => language.text("Git 历史帮助", "Git help · History"),
        HelpPage::File => language.text("文件帮助", "File help"),
    };
    heading(&mut out, title, width);
    row(
        &mut out,
        "",
        language.text(
            "↑↓ / 滚轮滚动 · PgUp / PgDn 翻页",
            "↑↓ / wheel: scroll · PgUp / PgDn: page",
        ),
        width,
    );
    row(
        &mut out,
        "",
        language.text(
            "? / Esc 返回 · l 切换语言",
            "? / Esc: back · l: 中文 / English",
        ),
        width,
    );

    match page {
        HelpPage::File => {
            for (key, chinese, english) in [
                (
                    "Alt+1/2/3",
                    "切换 Token / Git / File，保留各页位置",
                    "Select Token / Git / File; retain each page's position",
                ),
                (
                    "↑↓ / ←→",
                    "上下选择条目；左键返回上级，右键进入选中目录（文件不触发打开）。Enter 打开文件或目录，⌫ 也可返回上级；可双击打开、点击路径跳转",
                    "Up/down select; Left goes to parent, Right enters the selected directory (files stay selected). Enter opens a file or directory; Backspace also goes up. Double-click to open, click breadcrumbs to navigate",
                ),
                (
                    "右键 / o",
                    "底部弹出 Actions，文件列表保留在背景；点击操作，Esc 或点击外部关闭。输入与删除确认也在底部弹窗，Back 返回菜单",
                    "Bottom Actions popup keeps files in the background; click an action, Esc or outside click to close. Input and delete dialogs also open at the bottom; Back returns to Actions",
                ),
                (
                    "滚轮 / 滚动条",
                    "滚动文件列表；可点击或拖动右侧滚动条，后台刷新不阻塞操作",
                    "Wheel / scrollbar: scroll files; click or drag the rail. Background refresh stays interactive",
                ),
                (
                    "Settings → Pulse",
                    "在 Mux TUI 的 Settings → Pulse → File icons 选择 Nerd / Unicode，左右键更改、Ctrl+S 保存；File 自动更新并保留浏览位置。Unicode 使用 emoji，Nerd 需要 Nerd Font；图标固定两列",
                    "Mux TUI: Settings → Pulse → File icons; Left/Right selects Nerd / Unicode, Ctrl+S saves. File updates automatically and keeps its position. Unicode uses emoji; Nerd requires a Nerd Font. Icons occupy two columns",
                ),
                (
                    "p",
                    "固定目录；手动导航自动固定，取消后跟随原始 tab 的终端目录",
                    "Pin directory; navigation pins automatically, unpin to follow the originating tab",
                ),
                (
                    "/ / . / r",
                    "点击列表顶部的单行筛选框或按 / 输入，实时筛选；Enter 完成，Esc 恢复，[x] 或 Ctrl+U 清空。./r 显示隐藏文件/刷新",
                    "Click the top filter field or press /; filter live. Enter finishes, Esc restores, [x] / Ctrl+U clears. ./r: hidden files / refresh",
                ),
                (
                    "n / N / R",
                    "新建文件 / 新建目录 / 重命名",
                    "New file / new directory / rename",
                ),
                (
                    "c / m",
                    "复制 / 移动到新路径；相对当前目录或绝对路径，不覆盖同名目标",
                    "Copy / move to a new relative or absolute path; never overwrite",
                ),
                (
                    "d",
                    "确认后永久删除；目录连同内容删除，默认取消；符号链接仅删除链接",
                    "Confirm permanent deletion; directories include contents, cancel by default; links delete only the link",
                ),
                (
                    "VISUAL / EDITOR",
                    "选择编辑器，未配置时依次使用 nvim / vim / vi；支持附带参数",
                    "Editor configuration, including arguments; default nvim / vim / vi",
                ),
                (
                    "H / q / Ctrl+C",
                    "返回 File 首页（当前终端目录）/ 同上 / 退出；表单中 q、H 是文字",
                    "File Home (terminal directory) / same / quit; q and H are text in forms",
                ),
            ] {
                row(&mut out, key, language.text(chinese, english), width);
            }
        }

        HelpPage::Usage | HelpPage::Sessions | HelpPage::Charts => {
            row(
                &mut out,
                "a / AI",
                language.text(
                    "Sessions: 原客户端继续或跨 AI 交接；A 接管当前会话",
                    "Sessions: resume or hand off; A takes over the current session",
                ),
                width,
            );
            heading(&mut out, language.text("常用操作", "Shortcuts"), width);
            row(
                &mut out,
                "Tab / 1–5",
                language.text(
                    "筛选 Claude / Codex / Grok / 全部 / Pi",
                    "Filter Claude / Codex / Grok / All / Pi",
                ),
                width,
            );
            row(
                &mut out,
                "s / c",
                language.text(
                    "会话 / 图表；再按返回首页",
                    "Sessions / Charts; press again for Home",
                ),
                width,
            );
            match page {
                HelpPage::Usage => {
                    row(
                        &mut out,
                        "m",
                        language.text("服务商 / 模型明细", "Provider / model details"),
                        width,
                    );
                    row(
                        &mut out,
                        "a",
                        language.text(
                            "切换 Codex / Grok 已保存账号",
                            "Switch saved Codex / Grok accounts",
                        ),
                        width,
                    );
                    row(
                        &mut out,
                        "",
                        language.text(
                            "↑↓ 选择，Enter 复核，再按 Enter 确认",
                            "↑↓ select; Enter review, then Enter confirm",
                        ),
                        width,
                    );
                }
                HelpPage::Sessions => row(
                    &mut out,
                    "t",
                    language.text(
                        "按最近活动 / Token 用量排序",
                        "Sort by recent activity / tokens",
                    ),
                    width,
                ),
                HelpPage::Charts => {}
                _ => unreachable!(),
            }
            if matches!(page, HelpPage::Sessions) {
                row(
                    &mut out,
                    "/",
                    language.text(
                        "搜索标题、项目路径、会话 ID 或模型；Enter 完成，Ctrl+U 清空，Esc 取消编辑。",
                        "Search title, project, ID or model; Enter accepts, Ctrl+U clears, Esc cancels editing.",
                    ),
                    width,
                );
                row(&mut out, "↑↓ / Enter", language.text("选择会话；Enter 或点击打开详情，q / Esc 返回列表。滚轮滚动列表。", "Select a session; Enter or click opens details, q / Esc returns to the list. Wheel scrolls the list."), width);
                row(
                    &mut out,
                    "Esc",
                    language.text(
                        "列表中清除搜索；浏览历史时保留筛选和位置。",
                        "Clear search in the list; historical browsing keeps filter and position.",
                    ),
                    width,
                );
            }
            row(
                &mut out,
                "v / r",
                language.text(
                    "文字 / 图形显示；立即刷新",
                    "Text / visual view; refresh now",
                ),
                width,
            );
            row(
                &mut out,
                "g",
                language.text(
                    "切换 Git；Alt+1 / Alt+2 / Alt+3 直选用量 / Git / File",
                    "Switch to Git; Alt+1 / Alt+2 / Alt+3: Token / Git / File",
                ),
                width,
            );
            row(
                &mut out,
                "e / q",
                language.text("e 打开完整 Mux；q 内层逐级返回，仅首页关闭。Ctrl+C 直接退出。", "e opens full Mux; q goes back in inner pages and closes only on Home. Ctrl+C exits directly."),
                width,
            );

            heading(&mut out, language.text("统计说明", "Data scope"), width);
            match page {
                HelpPage::Usage => {
                    row(
                        &mut out,
                        language.text("今日网关", "Gateway today"),
                        language.text(
                            "仅统计经过 Mux 代理的请求，不含直连与订阅流量。",
                            "Mux proxy traffic only. Direct API and subscription traffic excluded.",
                        ),
                        width,
                    );
                    row(
                        &mut out,
                        language.text("当前会话", "Current session"),
                        language.text(
                            "聚焦终端的本地日志累计用量，与网关不相加。",
                            "Local log totals for the focused agent. Separate from gateway usage.",
                        ),
                        width,
                    );
                    row(
                        &mut out,
                        language.text("订阅模式", "Subscription"),
                        language.text(
                            "Codex / Grok 使用订阅时，首页自动精简为额度与会话。",
                            "Codex / Grok subscription mode shows quota and session usage on Home.",
                        ),
                        width,
                    );
                    row(
                        &mut out,
                        language.text("账号额度", "Quota"),
                        language.text("Codex 按 r 查询；Grok 页面每分钟刷新。切换后自动更新。", "Codex: r to query. Grok: every minute on its page. Updates after switching."),
                        width,
                    );
                    row(
                        &mut out,
                        language.text("成功率", "Success rate"),
                        language.text(
                            "成功 ÷（成功＋失败＋中断），不含待完成请求。",
                            "Success / (success + failed + interrupted). Pending calls excluded.",
                        ),
                        width,
                    );
                    row(
                        &mut out,
                        language.text("缓存 / 速率", "Cache / speed"),
                        language.text("命中率统计成功生成请求：缓存读 ÷ 全部输入，写入不算命中。速率含首字等待与网络耗时。", "Cache hit: reads / all input in successful generation calls; writes excluded. Speed includes first-token wait and network time."),
                        width,
                    );
                }
                HelpPage::Sessions => {
                    row(&mut out, language.text("会话标题", "Session title"), language.text("只读客户端保存的名称；缺少标题时显示项目名与短 ID，不从对话正文生成。", "Read-only client title metadata; fallback to project and short ID. Never generated from conversation bodies."), width);
                    row(
                        &mut out,
                        language.text("会话用量", "Session usage"),
                        language.text(
                            "来自本地日志，显示整个会话累计值，不限于今日。",
                            "Local log totals over the whole session, not just today.",
                        ),
                        width,
                    );
                    row(
                        &mut out,
                        language.text("当前会话", "Current session"),
                        language.text(
                            "跟随聚焦的 agent；其他会话列在下方。",
                            "Follows agent focus; other sessions appear below.",
                        ),
                        width,
                    );
                    row(
                        &mut out,
                        language.text("缓存复用", "Cache reuse"),
                        language.text(
                            "缓存读取 ÷ 全部输入；写入不算命中。",
                            "Cached reads / all input. Writes are not hits.",
                        ),
                        width,
                    );
                    row(
                        &mut out,
                        "Fork *",
                        language.text(
                            "可能含继承用量，子会话单独显示。",
                            "May include inherited usage; child sessions are listed separately.",
                        ),
                        width,
                    );
                }
                HelpPage::Charts => {
                    row(
                        &mut out,
                        language.text("上方图表", "Top chart"),
                        language.text("今日网关每小时请求数。", "Gateway requests per hour today."),
                        width,
                    );
                    row(
                        &mut out,
                        language.text("下方图表", "Lower chart"),
                        language.text(
                            "当前会话今日每小时 Token 增量（输入＋输出）。",
                            "Current session tokens added per hour today (input + output).",
                        ),
                        width,
                    );
                    row(
                        &mut out,
                        language.text("柱高", "Bar height"),
                        language.text(
                            "两图各自缩放，不能直接比较。",
                            "Each chart scales independently; heights are not comparable.",
                        ),
                        width,
                    );
                }
                _ => unreachable!(),
            }
            heading(
                &mut out,
                language.text("显示与跟随", "Display & focus"),
                width,
            );
            row(
                &mut out,
                language.text("自动跟随", "Auto follow"),
                language.text(
                    "切换 agent 焦点时更新当前会话；Sessions 保留筛选与浏览位置。",
                    "Agent focus updates the current session; Sessions retains its filter and browsing position.",
                ),
                width,
            );
            row(
                &mut out,
                language.text("图形标记", "Visual key"),
                language.text(
                    "蓝色输入、金色输出；R / W 表示缓存读 / 写。",
                    "Blue: input; gold: output. R / W: cache reads / writes.",
                ),
                width,
            );
            row(
                &mut out,
                language.text("缺失数据", "Missing data"),
                language.text(
                    "— / unknown 表示未知；stale 保留旧数据，按 r 重试。",
                    "— / unknown: missing data. stale: previous data kept; r to retry.",
                ),
                width,
            );
        }
        HelpPage::Git => {
            heading(
                &mut out,
                language.text("浏览与定位", "Browse & find"),
                width,
            );
            row(
                &mut out,
                "↑↓ / Enter",
                language.text(
                    "选择文件 / 打开差异；Esc 返回列表",
                    "Select file / open Diff; Esc returns to Files",
                ),
                width,
            );
            row(
                &mut out,
                "/ · p · i",
                language.text(
                    "顶部常驻 Search 方框实时筛选路径；点击或 / 输入，Enter 完成，Esc 恢复，[x] / Ctrl+U 清空。p 固定；点击项目名在名称下展开详情，i 查看详情，点击分支名在名称下展开菜单并确认切换",
                    "Persistent top Search field: click or / to filter paths live; Enter accepts, Esc restores, [x] / Ctrl+U clears. p pins; repository name opens details below it, i shows details; branch name opens a menu below it, with switch confirmation",
                ),
                width,
            );
            row(
                &mut out,
                "",
                language.text("点击分组标题折叠；未固定时跟随终端目录。", "Click group headings to collapse. Unpinned repos follow the terminal directory."),
                width,
            );
            row(
                &mut out,
                ", / .",
                language.text(
                    "最底行 Prev / Next：切换文件或 Log 历史页",
                    "Bottom Prev / Next: switch files or Log pages",
                ),
                width,
            );
            row(
                &mut out,
                "w / [ / ]",
                language.text(
                    "Actions 中切换换行 / 上一个差异块 / 下一个差异块",
                    "Wrapping in Actions / previous hunk / next hunk",
                ),
                width,
            );
            row(
                &mut out,
                "← / →",
                language.text("关闭换行后横向滚动", "Pan horizontally with wrapping off"),
                width,
            );

            heading(
                &mut out,
                language.text("修改与提交", "Changes & commit"),
                width,
            );
            row(
                &mut out,
                "s / h",
                language.text(
                    "暂存整文件 / 当前差异块",
                    "Stage whole file / selected hunk",
                ),
                width,
            );
            row(
                &mut out,
                "u / d",
                language.text(
                    "取消暂存 / 丢弃文件或当前差异块",
                    "Unstage / discard file or selected hunk",
                ),
                width,
            );
            row(
                &mut out,
                language.text("a 或 A / U", "a or A / U"),
                language.text("暂存全部 / 取消暂存全部", "Stage all / unstage all"),
                width,
            );
            row(
                &mut out,
                "D",
                language.text("丢弃整个未暂存文件", "Discard whole unstaged file"),
                width,
            );
            row(
                &mut out,
                "n",
                language.text(
                    "提交已暂存内容；Enter 换行",
                    "Commit staged changes; Enter adds a line",
                ),
                width,
            );
            row(
                &mut out,
                "",
                language.text(
                    "Ctrl+Enter / Review 复核，Enter 确认；Esc 返回修改。",
                    "Ctrl+Enter / Review to review; Enter confirms, Esc edits.",
                ),
                width,
            );

            heading(
                &mut out,
                language.text("分支与同步", "Branches & sync"),
                width,
            );
            row(
                &mut out,
                "b / L / o",
                language.text(
                    "分支选择 / 提交历史 / Actions 按 Files / Diff / Log 列出对应操作，底部仅 Choose / Back；Esc 或点击外部关闭，输入与确认 Esc 返回菜单",
                    "Branch selection / history / Actions follows Files / Diff / Log, with only Choose / Back below; Esc or outside click closes. Esc from input or confirmation returns to Actions",
                ),
                width,
            );
            row(
                &mut out,
                "r / l",
                language.text(
                    "刷新 / 最后一次操作的输出与错误",
                    "Refresh / last operation output & errors",
                ),
                width,
            );
            row(
                &mut out,
                "g / T",
                language.text(
                    "返回用量；Alt+1 / Alt+2 直选页面",
                    "Return to Token; Alt+1 / Alt+2 select pages",
                ),
                width,
            );
            row(
                &mut out,
                "",
                language.text("差异自动刷新。特殊文件仅支持整文件操作；冲突需在外部解决，Pull 仅快进。", "Diff refreshes automatically. Special files need whole-file actions. Resolve conflicts externally; Pull is fast-forward only."),
                width,
            );
        }
        HelpPage::Log => {
            heading(
                &mut out,
                language.text("历史操作", "History shortcuts"),
                width,
            );
            row(
                &mut out,
                "↑↓ / Enter",
                language.text(
                    "单击选择提交；双击或 Enter 进入下一层详情页查看消息、统计与差异",
                    "Click to select; double-click or Enter opens the full-page message, stats and Diff",
                ),
                width,
            );
            row(
                &mut out,
                ", / .",
                language.text(
                    "较新 / 较旧一页（每页 50 条）",
                    "Newer / older page (50 commits each)",
                ),
                width,
            );
            row(
                &mut out,
                "/",
                language.text(
                    "顶部常驻 Search 方框搜索提交标题与正文，点击或 / 输入，Enter 应用",
                    "Persistent top Search field for title and body; click or / to edit, Enter applies",
                ),
                width,
            );
            row(
                &mut out,
                "r / l / o",
                language.text(
                    "刷新 / 操作结果 / 全部操作",
                    "Refresh / operation result / all operations",
                ),
                width,
            );
            row(
                &mut out,
                "Esc",
                language.text(
                    "详情返回历史；历史返回文件列表",
                    "Details to history; history to Files",
                ),
                width,
            );
            row(
                &mut out,
                "g / T",
                language.text(
                    "返回用量；Alt+1 / Alt+2 直选页面",
                    "Return to Token; Alt+1 / Alt+2 select pages",
                ),
                width,
            );
            heading(&mut out, language.text("说明", "About"), width);
            row(
                &mut out,
                "",
                language.text(
                    "显示当前分支的历史，最新在前；详情自动换行。",
                    "Current branch history, newest first. Details wrap automatically.",
                ),
                width,
            );
        }
    }
    row(&mut out, "q / Ctrl+C", language.text("q 非输入状态逐级返回，Home(H) 返回当前功能首页，Git 根页保持在 Git；表单与搜索中 q 是文字，执行中不能返回。Ctrl+C 直接退出。", "q goes back outside text input; Home(H) returns to the current feature home; Git root stays in Git. In forms/search, q is text; busy operations block back. Ctrl+C exits directly."), width);
    out
}

fn heading(out: &mut Vec<Line<'static>>, title: &str, width: u16) {
    if !out.is_empty() {
        out.push(Line::default());
    }
    if title.width() < usize::from(width) {
        out.push(section(title, width));
    } else {
        out.extend(
            wrapped(title, usize::from(width.max(1)), INK, false)
                .into_iter()
                .map(|mut line| {
                    for span in &mut line.spans {
                        span.style = span.style.add_modifier(Modifier::BOLD);
                    }
                    line
                }),
        );
    }
}

fn row(out: &mut Vec<Line<'static>>, key: &str, text: &str, width: u16) {
    let width = usize::from(width.max(1));
    if key.is_empty() {
        out.extend(wrapped(text, width, SOFT, true));
    } else if key.width() + 6 > width {
        out.extend(wrapped(key, width, BLUE, false));
        out.extend(wrapped(text, width, SOFT, true));
    } else {
        let gap = if width >= 48 {
            14usize.saturating_sub(key.width()).max(2)
        } else {
            2
        };
        let mut spans = vec![
            Span::styled(key.to_owned(), Style::default().fg(BLUE)),
            Span::raw(" ".repeat(gap)),
        ];
        let mut current = String::new();
        let mut used = key.width() + gap;
        for (index, ch) in text.char_indices() {
            let size = ch.width().unwrap_or(0);
            if used + size > width
                || (used > 2 && word_needs_wrap(text, index, used, width, width.saturating_sub(2)))
            {
                spans.push(Span::styled(
                    std::mem::take(&mut current),
                    Style::default().fg(SOFT),
                ));
                out.push(Line::from(std::mem::take(&mut spans)));
                current.push_str("  ");
                used = 2;
            }
            current.push(ch);
            used += size;
        }
        spans.push(Span::styled(current, Style::default().fg(SOFT)));
        out.push(Line::from(spans));
    }
}

fn wrapped(text: &str, width: usize, color: Color, indent: bool) -> Vec<Line<'static>> {
    let prefix = if indent && width >= 4 { "  " } else { "" };
    let available = width.saturating_sub(prefix.len()).max(1);
    let mut out = vec![];
    let mut current = prefix.to_owned();
    let mut used = 0;
    for (index, ch) in text.char_indices() {
        let size = ch.width().unwrap_or(0);
        if used > 0
            && (used + size > available || word_needs_wrap(text, index, used, available, available))
        {
            out.push(line(
                std::mem::replace(&mut current, prefix.to_owned()),
                color,
            ));
            used = 0;
        }
        current.push(ch);
        used += size;
    }
    out.push(line(current, color));
    out
}

fn word_needs_wrap(text: &str, index: usize, used: usize, width: usize, available: usize) -> bool {
    if !text[..index]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace)
    {
        return false;
    }
    let word = text[index..]
        .chars()
        .take_while(|ch| ch.is_ascii() && !ch.is_whitespace())
        .count();
    word > 0 && word <= available && used + word > width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bilingual_help_wraps_without_losing_content_or_key_styles() {
        for page in [
            HelpPage::Usage,
            HelpPage::Sessions,
            HelpPage::Charts,
            HelpPage::Git,
            HelpPage::Log,
        ] {
            let body = |lines: &[Line<'_>]| -> String {
                lines
                    .iter()
                    .filter(|line| {
                        !line
                            .spans
                            .first()
                            .is_some_and(|s| s.style.add_modifier.contains(Modifier::BOLD))
                    })
                    .flat_map(|l| l.to_string().chars().collect::<Vec<_>>())
                    .filter(|ch| !ch.is_whitespace() && *ch != '─')
                    .collect()
            };
            for language in [HelpLanguage::Chinese, HelpLanguage::English] {
                let expected = body(&help_content(page, language, 80));
                for width in [8, 20, 28, 44, 76] {
                    let lines = help_content(page, language, width);
                    assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
                    assert_eq!(body(&lines), expected, "help width {width}");
                    assert!(
                        lines
                            .iter()
                            .flat_map(|l| &l.spans)
                            .any(|s| s.style.fg == Some(BLUE))
                    );
                }
            }
        }
    }

    #[test]
    fn english_words_and_language_controls_fit_narrow_sidebars() {
        let lines = wrapped("Local subscription traffic stays separate.", 22, SOFT, true);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("subscription"))
        );
        for width in [8, 20, 28, 44, 76] {
            for language in [HelpLanguage::Chinese, HelpLanguage::English] {
                for button in help_footer(Rect::new(2, 0, width, 12), language) {
                    assert!(button.label.width() <= usize::from(button.rect.width));
                    assert!(button.rect.right() <= width + 2);
                }
            }
        }
    }
}
