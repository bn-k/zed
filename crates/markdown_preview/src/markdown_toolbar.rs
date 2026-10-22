use std::ops::Range;

use editor::{Editor, MultiBufferOffset, SelectionEffects};
use gpui::{Entity, EventEmitter, Focusable, FontWeight, Subscription, WeakEntity};
use ui::{ButtonLike, Divider, Tooltip, prelude::*};
use workspace::{ItemHandle, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace};

use crate::{CloseAndReturnToEditor, markdown_preview_view::MarkdownPreviewView};

/// A Markdown construct the toolbar writes around the selections, or at the start of their lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Heading(usize),
    Bold,
    Italic,
    Strikethrough,
    Code,
    WikiLink,
    BulletList,
    NumberedList,
    TaskList,
    Quote,
}

impl Format {
    fn markers(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Bold => Some(("**", "**")),
            Self::Italic => Some(("_", "_")),
            Self::Strikethrough => Some(("~~", "~~")),
            Self::Code => Some(("`", "`")),
            Self::WikiLink => Some(("[[", "]]")),
            _ => None,
        }
    }

    fn line_prefix(self, number: usize) -> String {
        match self {
            Self::Heading(level) => format!("{} ", "#".repeat(level)),
            Self::BulletList => "- ".into(),
            Self::NumberedList => format!("{number}. "),
            Self::TaskList => "- [ ] ".into(),
            Self::Quote => "> ".into(),
            _ => String::new(),
        }
    }
}

/// The edits that apply `format` to `selections` of `text`, in the coordinates of `text`, and the
/// selections afterwards, in the coordinates of the edited text. Applied again to the selections
/// it returns, a format is removed.
pub fn format_edits(
    format: Format,
    text: &str,
    selections: &[Range<usize>],
) -> (Vec<(Range<usize>, String)>, Vec<Range<usize>>) {
    match format.markers() {
        Some((open, close)) => wrap(text, selections, open, close),
        None => prefix_lines(format, text, selections),
    }
}

fn wrap(
    text: &str,
    selections: &[Range<usize>],
    open: &str,
    close: &str,
) -> (Vec<(Range<usize>, String)>, Vec<Range<usize>>) {
    let mut edits = Vec::new();
    let mut new_selections = Vec::new();
    let mut delta = 0isize;
    for selection in merged(selections) {
        let (start, end) = (selection.start, selection.end);
        let selected = &text[start..end];
        let shift = |offset: usize, delta: isize| (offset as isize + delta) as usize;
        if selected.len() >= open.len() + close.len()
            && selected.starts_with(open)
            && selected.ends_with(close)
        {
            edits.push((start..start + open.len(), String::new()));
            edits.push((end - close.len()..end, String::new()));
            new_selections.push(shift(start, delta)..shift(end - open.len() - close.len(), delta));
            delta -= (open.len() + close.len()) as isize;
        } else if text[..start].ends_with(open) && text[end..].starts_with(close) {
            edits.push((start - open.len()..start, String::new()));
            edits.push((end..end + close.len(), String::new()));
            new_selections.push(shift(start - open.len(), delta)..shift(end - open.len(), delta));
            delta -= (open.len() + close.len()) as isize;
        } else if start == end {
            edits.push((start..start, format!("{open}{close}")));
            new_selections.push(shift(start + open.len(), delta)..shift(end + open.len(), delta));
            delta += (open.len() + close.len()) as isize;
        } else {
            edits.push((start..start, open.to_string()));
            edits.push((end..end, close.to_string()));
            new_selections.push(shift(start + open.len(), delta)..shift(end + open.len(), delta));
            delta += (open.len() + close.len()) as isize;
        }
    }
    (edits, new_selections)
}

fn prefix_lines(
    format: Format,
    text: &str,
    selections: &[Range<usize>],
) -> (Vec<(Range<usize>, String)>, Vec<Range<usize>>) {
    let mut line_starts = Vec::new();
    for selection in selections {
        let mut line_start = text[..selection.start].rfind('\n').map_or(0, |ix| ix + 1);
        loop {
            if !line_starts.contains(&line_start) {
                line_starts.push(line_start);
            }
            let Some(newline) = text[line_start..].find('\n') else {
                break;
            };
            line_start += newline + 1;
            if line_start >= selection.end {
                break;
            }
        }
    }
    line_starts.sort();
    if line_starts.len() > 1 {
        line_starts.retain(|&start| !line_at(text, start).trim().is_empty());
    }

    let prefixes = line_starts
        .iter()
        .map(|&start| {
            let line = line_at(text, start);
            let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
            let (kind, len) = line_prefix(&line[indent..]);
            (start + indent, kind, len)
        })
        .collect::<Vec<_>>();
    let remove = prefixes.iter().all(|(_, kind, _)| *kind == Some(format));

    let mut edits = Vec::new();
    for (number, (start, _, len)) in prefixes.into_iter().enumerate() {
        let prefix = if remove {
            String::new()
        } else {
            format.line_prefix(number + 1)
        };
        if text[start..start + len] != prefix {
            edits.push((start..start + len, prefix));
        }
    }
    let new_selections = selections
        .iter()
        .map(|selection| {
            // A selection takes in the markers written at its start; a cursor goes after them.
            let start = map_offset(&edits, selection.start, !selection.is_empty());
            start..map_offset(&edits, selection.end, false).max(start)
        })
        .collect();
    (edits, new_selections)
}

fn line_at(text: &str, start: usize) -> &str {
    let rest = &text[start..];
    &rest[..rest.find('\n').unwrap_or(rest.len())]
}

/// The construct a line starts with, after its indentation, and the length of its marker.
fn line_prefix(line: &str) -> (Option<Format>, usize) {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        return (Some(Format::Heading(hashes)), hashes + 1);
    }
    for task in ["- [ ] ", "- [x] ", "- [X] "] {
        if line.starts_with(task) {
            return (Some(Format::TaskList), task.len());
        }
    }
    for bullet in ["- ", "* ", "+ "] {
        if line.starts_with(bullet) {
            return (Some(Format::BulletList), bullet.len());
        }
    }
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 && (line[digits..].starts_with(". ") || line[digits..].starts_with(") ")) {
        return (Some(Format::NumberedList), digits + 2);
    }
    if line.starts_with("> ") {
        return (Some(Format::Quote), 2);
    }
    (None, 0)
}

/// Where `offset` lands once `edits`, sorted and disjoint, are applied. An offset inside a
/// replaced range stays inside the replacement, and an offset where text is inserted goes after
/// it, unless `before_insertions`.
fn map_offset(edits: &[(Range<usize>, String)], offset: usize, before_insertions: bool) -> usize {
    let mut delta = 0isize;
    for (range, new_text) in edits {
        if range.start > offset || (before_insertions && range.is_empty() && range.start == offset)
        {
            break;
        }
        if range.end <= offset {
            delta += new_text.len() as isize - range.len() as isize;
        } else if range.start < offset {
            let inside = (offset - range.start).min(new_text.len());
            return (range.start as isize + delta) as usize + inside;
        }
    }
    (offset as isize + delta) as usize
}

fn merged(selections: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut sorted = selections.to_vec();
    sorted.sort_by_key(|selection| selection.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for selection in sorted {
        if let Some(previous) = merged.last_mut()
            && selection.start < previous.end
        {
            previous.end = previous.end.max(selection.end);
        } else {
            merged.push(selection);
        }
    }
    merged
}

/// Applies `format` to the selections of `editor`, an editor of a single buffer.
pub fn apply_format(editor: &Entity<Editor>, format: Format, window: &mut Window, cx: &mut App) {
    editor.update(cx, |editor, cx| {
        let text = editor.text(cx);
        let selections = editor
            .selections
            .all::<MultiBufferOffset>(&editor.display_snapshot(cx))
            .into_iter()
            .map(|selection| selection.start.0..selection.end.0)
            .collect::<Vec<_>>();
        let (edits, new_selections) = format_edits(format, &text, &selections);
        editor.transact(window, cx, |editor, window, cx| {
            editor.edit(
                edits.into_iter().map(|(range, new_text)| {
                    (
                        MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
                        new_text,
                    )
                }),
                cx,
            );
            editor.change_selections(SelectionEffects::default(), window, cx, |selections| {
                selections.select_ranges(new_selections.into_iter().map(|selection| {
                    MultiBufferOffset(selection.start)..MultiBufferOffset(selection.end)
                }))
            });
        });
    });
    editor.focus_handle(cx).focus(window, cx);
}

enum ToolbarTarget {
    Source(Entity<Editor>),
    Preview(Entity<MarkdownPreviewView>),
}

/// The header of a Markdown document: switches between its source and its preview, and writes
/// Markdown constructs in the source, or in the block of the preview edited in place.
pub struct MarkdownToolbar {
    workspace: WeakEntity<Workspace>,
    target: Option<ToolbarTarget>,
    _observe_target: Option<Subscription>,
}

impl MarkdownToolbar {
    pub fn new(workspace: &Workspace) -> Self {
        Self {
            workspace: workspace.weak_handle(),
            target: None,
            _observe_target: None,
        }
    }

    /// The editor the formatting buttons write into, if any.
    fn format_target(&self, cx: &App) -> Option<Entity<Editor>> {
        match self.target.as_ref()? {
            ToolbarTarget::Source(editor) => Some(editor.clone()),
            ToolbarTarget::Preview(preview) => preview.read(cx).inline_editor(),
        }
    }

    // Switching views changes the pane's active item, which updates this toolbar: both switches
    // run once the click is handled.

    fn show_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ToolbarTarget::Preview(preview)) = &self.target else {
            return;
        };
        let preview = preview.clone();
        window.defer(cx, move |window, cx| {
            preview.update(cx, |preview, cx| {
                preview.close_and_return_to_editor(&CloseAndReturnToEditor, window, cx)
            });
        });
    }

    fn show_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ToolbarTarget::Source(editor)) = &self.target else {
            return;
        };
        let editor = editor.clone();
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    let Some(pane) = workspace.pane_for(&editor) else {
                        return;
                    };
                    MarkdownPreviewView::open_preview_in_pane(workspace, editor, pane, window, cx);
                })
                .ok();
        });
    }

    fn render_format_button(
        &self,
        id: &'static str,
        label: impl IntoElement,
        tooltip: &'static str,
        format: Format,
        target: Option<Entity<Editor>>,
    ) -> impl IntoElement {
        div().debug_selector(|| id.into()).child(
            ButtonLike::new(id)
                .style(ButtonStyle::Subtle)
                .size(ButtonSize::Compact)
                .disabled(target.is_none())
                .child(label)
                .tooltip(Tooltip::text(tooltip))
                .on_click(move |_, window, cx| {
                    if let Some(editor) = &target {
                        apply_format(editor, format, window, cx);
                    }
                }),
        )
    }
}

impl Render for MarkdownToolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let showing_preview = matches!(self.target, Some(ToolbarTarget::Preview(_)));
        let target = self.format_target(cx);
        let text = |text: &'static str| Label::new(text).size(LabelSize::Small);
        let icon = |icon: IconName| Icon::new(icon).size(IconSize::Small);

        h_flex()
            .id("markdown-toolbar")
            .w_full()
            .gap_1()
            .child(
                div()
                    .debug_selector(|| "markdown-show-source".into())
                    .child(
                        Button::new("markdown-show-source", "Source")
                            .label_size(LabelSize::Small)
                            .toggle_state(!showing_preview)
                            .tooltip(Tooltip::text("Edit the source"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.show_source(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "markdown-show-preview".into())
                    .child(
                        Button::new("markdown-show-preview", "Preview")
                            .label_size(LabelSize::Small)
                            .toggle_state(showing_preview)
                            .tooltip(Tooltip::text("Read laid out; click a block to edit it"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.show_preview(window, cx)),
                            ),
                    ),
            )
            .child(Divider::vertical().mx_1())
            .child(self.render_format_button(
                "markdown-heading-1",
                text("H1"),
                "Heading 1",
                Format::Heading(1),
                target.clone(),
            ))
            .child(self.render_format_button(
                "markdown-heading-2",
                text("H2"),
                "Heading 2",
                Format::Heading(2),
                target.clone(),
            ))
            .child(self.render_format_button(
                "markdown-heading-3",
                text("H3"),
                "Heading 3",
                Format::Heading(3),
                target.clone(),
            ))
            .child(Divider::vertical().mx_1())
            .child(self.render_format_button(
                "markdown-bold",
                text("B").weight(FontWeight::BOLD),
                "Bold",
                Format::Bold,
                target.clone(),
            ))
            .child(self.render_format_button(
                "markdown-italic",
                text("I").italic(),
                "Italic",
                Format::Italic,
                target.clone(),
            ))
            .child(self.render_format_button(
                "markdown-strikethrough",
                text("S").strikethrough(),
                "Strikethrough",
                Format::Strikethrough,
                target.clone(),
            ))
            .child(self.render_format_button(
                "markdown-code",
                icon(IconName::Code),
                "Code",
                Format::Code,
                target.clone(),
            ))
            .child(Divider::vertical().mx_1())
            .child(self.render_format_button(
                "markdown-bullet-list",
                text("•"),
                "Bulleted list",
                Format::BulletList,
                target.clone(),
            ))
            .child(self.render_format_button(
                "markdown-numbered-list",
                text("1."),
                "Numbered list",
                Format::NumberedList,
                target.clone(),
            ))
            .child(self.render_format_button(
                "markdown-task-list",
                icon(IconName::ListTodo),
                "Task list",
                Format::TaskList,
                target.clone(),
            ))
            .child(self.render_format_button(
                "markdown-quote",
                icon(IconName::Quote),
                "Quote",
                Format::Quote,
                target.clone(),
            ))
            .child(self.render_format_button(
                "markdown-wiki-link",
                icon(IconName::Link),
                "Link to a document",
                Format::WikiLink,
                target,
            ))
    }
}

impl EventEmitter<ToolbarItemEvent> for MarkdownToolbar {}

impl ToolbarItemView for MarkdownToolbar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        self.target = None;
        self._observe_target = None;
        let Some(item) = active_pane_item else {
            return ToolbarItemLocation::Hidden;
        };
        if let Some(preview) = item.downcast::<MarkdownPreviewView>() {
            self._observe_target = Some(cx.observe_in(&preview, window, |_, _, _, cx| cx.notify()));
            self.target = Some(ToolbarTarget::Preview(preview));
        } else if let Some(editor) = item.downcast::<Editor>()
            && MarkdownPreviewView::is_markdown_file(&editor, cx)
        {
            self.target = Some(ToolbarTarget::Source(editor));
        } else {
            return ToolbarItemLocation::Hidden;
        }
        ToolbarItemLocation::Secondary
    }
}

#[cfg(test)]
mod tests {
    use super::{Format, format_edits};

    /// Applies `format` to `text`, where `|` marks a cursor and `«…»` a selection, and writes the
    /// result the same way.
    fn format(format: Format, marked: &str) -> String {
        let mut text = String::new();
        let mut selections = Vec::new();
        let mut start = None;
        for c in marked.chars() {
            match c {
                '|' => selections.push(text.len()..text.len()),
                '«' => start = Some(text.len()),
                '»' => selections.push(start.take().unwrap()..text.len()),
                c => text.push(c),
            }
        }
        let (edits, new_selections) = format_edits(format, &text, &selections);
        for (range, new_text) in edits.into_iter().rev() {
            text.replace_range(range, &new_text);
        }
        for selection in new_selections.into_iter().rev() {
            if selection.is_empty() {
                text.insert(selection.start, '|');
            } else {
                text.insert(selection.end, '»');
                text.insert(selection.start, '«');
            }
        }
        text
    }

    #[test]
    fn wraps_and_unwraps_the_selection() {
        assert_eq!(format(Format::Bold, "a «word» here"), "a **«word»** here");
        assert_eq!(format(Format::Bold, "a **«word»** here"), "a «word» here");
        assert_eq!(format(Format::Bold, "a «**word**» here"), "a «word» here");
        assert_eq!(format(Format::Italic, "x|y"), "x_|_y");
        assert_eq!(
            format(Format::WikiLink, "see «agents/marc»"),
            "see [[«agents/marc»]]"
        );
        assert_eq!(format(Format::Code, "«a» and «b»"), "`«a»` and `«b»`");
    }

    #[test]
    fn prefixes_and_unprefixes_lines() {
        assert_eq!(
            format(Format::Heading(2), "Ti|tle\nbody"),
            "## Ti|tle\nbody"
        );
        assert_eq!(format(Format::Heading(2), "## Ti|tle"), "Ti|tle");
        assert_eq!(format(Format::Heading(1), "## Ti|tle"), "# Ti|tle");
        assert_eq!(format(Format::Heading(1), "|"), "# |");
        assert_eq!(format(Format::BulletList, "«a\n\nb»"), "«- a\n\n- b»");
        assert_eq!(format(Format::NumberedList, "«a\nb»"), "«1. a\n2. b»");
        assert_eq!(format(Format::NumberedList, "«1. a\n2. b»"), "«a\nb»");
        assert_eq!(format(Format::TaskList, "  - |item"), "  - [ ] |item");
        assert_eq!(format(Format::TaskList, "- [x] it|em"), "it|em");
        assert_eq!(format(Format::Quote, "|quoted"), "> |quoted");
    }
}
