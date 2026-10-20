use agent_settings::{AgentLabelStyle, AgentSettings};
use gpui::{Action, ClickEvent, FocusHandle, Hsla, prelude::*};
use language_model::DisabledReason;
use settings::Settings as _;
use ui::{
    Chip, ElevationIndex, KeyBinding, ListItem, ListItemSpacing, TintColor, Tooltip, prelude::*,
};
use zed_actions::agent::ToggleModelSelector;

use crate::CycleFavoriteModels;

enum ModelIcon {
    Name(IconName),
    Path(SharedString),
}

#[derive(IntoElement)]
pub struct ModelSelectorHeader {
    title: SharedString,
    has_border: bool,
}

impl ModelSelectorHeader {
    pub fn new(title: impl Into<SharedString>, has_border: bool) -> Self {
        Self {
            title: title.into(),
            has_border,
        }
    }
}

impl RenderOnce for ModelSelectorHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .px_2()
            .pb_1()
            .when(self.has_border, |this| {
                this.mt_1()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
            })
            .child(
                Label::new(self.title)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
    }
}

#[derive(IntoElement)]
pub struct ModelSelectorListItem {
    index: usize,
    title: SharedString,
    icon: Option<ModelIcon>,
    is_selected: bool,
    is_focused: bool,
    is_latest: bool,
    is_favorite: bool,
    disabled: Option<DisabledReason>,
    on_toggle_favorite: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    cost_info: Option<SharedString>,
}

impl ModelSelectorListItem {
    pub fn new(index: usize, title: impl Into<SharedString>) -> Self {
        Self {
            index,
            title: title.into(),
            icon: None,
            is_selected: false,
            is_focused: false,
            is_latest: false,
            is_favorite: false,
            disabled: None,
            on_toggle_favorite: None,
            cost_info: None,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(ModelIcon::Name(icon));
        self
    }

    pub fn icon_path(mut self, path: SharedString) -> Self {
        self.icon = Some(ModelIcon::Path(path));
        self
    }

    pub fn is_selected(mut self, is_selected: bool) -> Self {
        self.is_selected = is_selected;
        self
    }

    pub fn disabled(mut self, disabled: Option<DisabledReason>) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn is_focused(mut self, is_focused: bool) -> Self {
        self.is_focused = is_focused;
        self
    }

    pub fn is_latest(mut self, is_latest: bool) -> Self {
        self.is_latest = is_latest;
        self
    }

    pub fn is_favorite(mut self, is_favorite: bool) -> Self {
        self.is_favorite = is_favorite;
        self
    }

    pub fn on_toggle_favorite(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle_favorite = Some(Box::new(handler));
        self
    }

    pub fn cost_info(mut self, cost_info: Option<SharedString>) -> Self {
        self.cost_info = cost_info;
        self
    }
}

impl RenderOnce for ModelSelectorListItem {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let is_disabled = self.disabled.is_some();

        let model_icon_color = if self.is_selected {
            Color::Accent
        } else if is_disabled {
            Color::Disabled
        } else {
            Color::Muted
        };

        let is_favorite = self.is_favorite;

        ListItem::new(self.index)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(self.is_focused)
            .when_some(self.disabled, |this, disabled_reason| {
                this.disabled(true)
                    .tooltip(Tooltip::text(disabled_reason.0))
            })
            .child(
                h_flex()
                    .w_full()
                    .gap_1p5()
                    .when_some(self.icon, |this, icon| {
                        this.child(
                            match icon {
                                ModelIcon::Name(icon_name) => Icon::new(icon_name),
                                ModelIcon::Path(icon_path) => Icon::from_external_svg(icon_path),
                            }
                            .color(model_icon_color)
                            .size(IconSize::Small),
                        )
                    })
                    .child(
                        Label::new(self.title)
                            .when(is_disabled, |this| this.color(Color::Disabled))
                            .truncate(),
                    )
                    .when(self.is_latest, |parent| parent.child(Chip::new("Latest")))
                    .when_some(self.cost_info, |this, cost_info| {
                        let tooltip_text = if cost_info.ends_with('×') {
                            format!("Cost Multiplier: {}", cost_info)
                        } else if cost_info.contains('$') {
                            format!("Cost per Million Tokens: {}", cost_info)
                        } else {
                            format!("Cost: {}", cost_info)
                        };

                        this.child(Chip::new(cost_info).tooltip(Tooltip::text(tooltip_text)))
                    }),
            )
            .end_slot(
                h_flex()
                    .pr_2()
                    .gap_1p5()
                    .when(self.is_selected, |this| {
                        this.child(Icon::new(IconName::Check).color(Color::Accent))
                    })
                    .when(is_disabled, |this| {
                        this.child(Icon::new(IconName::Info).color(Color::Muted))
                    }),
            )
            .when(!is_disabled, |this| {
                this.end_slot_on_hover(div().pr_1p5().when_some(self.on_toggle_favorite, {
                    |this, handle_click| {
                        let (icon, color, tooltip) = if is_favorite {
                            (IconName::StarFilled, Color::Accent, "Unfavorite Model")
                        } else {
                            (IconName::Star, Color::Default, "Favorite Model")
                        };
                        this.child(
                            IconButton::new(("toggle-favorite", self.index), icon)
                                .layer(ElevationIndex::ElevatedSurface)
                                .icon_color(color)
                                .icon_size(IconSize::Small)
                                .tooltip(Tooltip::text(tooltip))
                                .on_click(move |event, window, cx| {
                                    (handle_click)(event, window, cx)
                                }),
                        )
                    }
                }))
            })
    }
}

#[derive(IntoElement)]
pub struct ModelSelectorFooter {
    action: Box<dyn Action>,
    focus_handle: FocusHandle,
}

impl ModelSelectorFooter {
    pub fn new(action: Box<dyn Action>, focus_handle: FocusHandle) -> Self {
        Self {
            action,
            focus_handle,
        }
    }
}

impl RenderOnce for ModelSelectorFooter {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let action = self.action;
        let focus_handle = self.focus_handle;

        h_flex()
            .w_full()
            .p_1p5()
            .border_t_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                Button::new("configure", "Configure")
                    .full_width()
                    .style(ButtonStyle::Outlined)
                    .key_binding(
                        KeyBinding::for_action_in(action.as_ref(), &focus_handle, cx)
                            .map(|kb| kb.size(rems_from_px(12_f32))),
                    )
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(action.boxed_clone(), cx);
                    }),
            )
    }
}

#[derive(IntoElement)]
pub struct ModelSelectorTooltip {
    show_cycle_row: bool,
}

impl ModelSelectorTooltip {
    pub fn new() -> Self {
        Self {
            show_cycle_row: true,
        }
    }

    pub fn show_cycle_row(mut self, show: bool) -> Self {
        self.show_cycle_row = show;
        self
    }
}

impl RenderOnce for ModelSelectorTooltip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(Label::new("Change Model"))
                    .child(KeyBinding::for_action(&ToggleModelSelector, cx)),
            )
            .when(self.show_cycle_row, |this| {
                this.child(
                    h_flex()
                        .pt_1()
                        .gap_2()
                        .border_t_1()
                        .border_color(cx.theme().colors().border_variant)
                        .justify_between()
                        .child(Label::new("Cycle Favorite Models"))
                        .child(KeyBinding::for_action(&CycleFavoriteModels, cx)),
                )
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelTier {
    Fast,
    Frontier,
}

/// Splits a model name, id, or settings key into lowercase words, so that
/// "GPT-5 mini" and "gpt-5-mini" compare equal and "gemini" is not read as "mini".
fn words(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Infers the tier from the words of a model name or id, so that the same rule
/// works across providers (e.g. "Claude Haiku 4.5", "gpt-5-mini", "gemini-2.5-pro").
fn model_tier(model_name: &str) -> Option<ModelTier> {
    const FAST_WORDS: &[&str] = &["haiku", "flash", "lite", "mini", "nano"];
    const FRONTIER_WORDS: &[&str] = &["opus", "fable", "ultra", "pro"];

    let words = words(model_name);
    let has_any =
        |candidates: &[&str]| words.iter().any(|word| candidates.contains(&word.as_str()));

    if has_any(FAST_WORDS) {
        Some(ModelTier::Fast)
    } else if has_any(FRONTIER_WORDS) {
        Some(ModelTier::Frontier)
    } else {
        None
    }
}

/// Finds the configured color whose key's words appear, in order, in one of the
/// model's names. The key with the most words wins, then the first configured.
fn configured_model_color(colors: &[(String, Hsla)], model_names: &[&str]) -> Option<Hsla> {
    let model_words = model_names
        .iter()
        .map(|name| words(name))
        .collect::<Vec<_>>();
    colors
        .iter()
        .rev()
        .filter_map(|(key, color)| {
            let key_words = words(key);
            let matches = !key_words.is_empty()
                && model_words.iter().any(|name_words| {
                    name_words
                        .windows(key_words.len())
                        .any(|window| window == key_words.as_slice())
                });
            matches.then_some((key_words.len(), *color))
        })
        .max_by_key(|(word_count, _)| *word_count)
        .map(|(_, color)| color)
}

/// How a model or thinking effort label is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LabelColor {
    /// The label's text is drawn in the color.
    Text(Color),
    /// The label sits on a background filled with the color.
    Tag(TintColor),
}

impl LabelColor {
    fn new(color: Hsla, style: AgentLabelStyle) -> Self {
        match style {
            AgentLabelStyle::Text => Self::Text(Color::Custom(color)),
            AgentLabelStyle::Tag => Self::Tag(TintColor::Custom(color)),
        }
    }

    pub fn label_color(self, cx: &App) -> Color {
        match self {
            Self::Text(color) => color,
            Self::Tag(tint) => Color::Custom(tint.label_color(cx)),
        }
    }

    /// Icons next to a text label keep their usual color; on a tag they take the
    /// label's color so they stay readable.
    pub fn icon_color(self, default: Color, cx: &App) -> Color {
        match self {
            Self::Text(_) => default,
            Self::Tag(tint) => Color::Custom(tint.label_color(cx)),
        }
    }

    pub fn button_style(self) -> ButtonStyle {
        match self {
            Self::Text(_) => ButtonStyle::default(),
            Self::Tag(tint) => ButtonStyle::Tinted(tint),
        }
    }
}

/// Colors the label of a model from the user's settings, falling back to a tint
/// by tier. `model_names` lists the names the model is known by, such as its
/// display name and its id. Returns `None` for labels that keep the default look.
pub fn model_label_color(model_names: &[&str], cx: &App) -> Option<LabelColor> {
    let settings = &AgentSettings::get_global(cx).label_colors.model;
    if let Some(color) = configured_model_color(&settings.colors, model_names) {
        return Some(LabelColor::new(color, settings.style));
    }

    let tier = model_names.iter().find_map(|name| model_tier(name))?;
    let status = cx.theme().status();
    let tier_color = match tier {
        ModelTier::Fast => status.success,
        ModelTier::Frontier => status.info,
    };
    let color = match settings.style {
        // Only lean toward the tier color so the label stays as quiet as its neighbors.
        AgentLabelStyle::Text => cx
            .theme()
            .colors()
            .text_muted
            .blend(tier_color.opacity(0.6)),
        AgentLabelStyle::Tag => tier_color,
    };
    Some(LabelColor::new(color, settings.style))
}

/// How far `selected_value` sits above the `high` effort level: `0.0` at `high`,
/// `1.0` at the highest level, and `None` at or below `medium`.
fn effort_heat(level_values: &[&str], selected_value: &str) -> Option<f32> {
    let high_index = level_values
        .iter()
        .position(|value| value.eq_ignore_ascii_case("high"))?;
    let selected_index = level_values
        .iter()
        .position(|value| *value == selected_value)?;
    let steps_above_high = selected_index.checked_sub(high_index)?;
    if steps_above_high == 0 {
        return Some(0.0);
    }
    let levels_above_high = level_values.len() - 1 - high_index;
    Some(steps_above_high as f32 / levels_above_high as f32)
}

/// Colors the label of the selected effort level from the user's settings, matched
/// by the level's value or name. Otherwise colors it by cost: levels up to `medium`
/// keep the default look, `high` is yellow, and higher levels shade toward red at
/// the maximum.
pub fn effort_label_color(
    level_values: &[&str],
    selected_value: &str,
    selected_name: &str,
    cx: &App,
) -> Option<LabelColor> {
    let settings = &AgentSettings::get_global(cx).label_colors.effort;
    let configured_color = settings.colors.iter().find_map(|(key, color)| {
        (key.eq_ignore_ascii_case(selected_value) || key.eq_ignore_ascii_case(selected_name))
            .then_some(*color)
    });
    let color = match configured_color {
        Some(color) => color,
        None => {
            let heat = effort_heat(level_values, selected_value)?;
            let status = cx.theme().status();
            status.warning.blend(status.error.opacity(heat))
        }
    };
    Some(LabelColor::new(color, settings.style))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_tier() {
        assert_eq!(model_tier("Claude Haiku 4.5"), Some(ModelTier::Fast));
        assert_eq!(model_tier("gpt-5-mini"), Some(ModelTier::Fast));
        assert_eq!(model_tier("gemini-2.5-flash-lite"), Some(ModelTier::Fast));
        assert_eq!(model_tier("Claude Opus 5.5"), Some(ModelTier::Frontier));
        assert_eq!(model_tier("gemini-2.5-pro"), Some(ModelTier::Frontier));
        assert_eq!(model_tier("Claude Sonnet 5.5"), None);
        assert_eq!(model_tier("gemini-3"), None);
    }

    #[test]
    fn test_configured_model_color() {
        let blue = gpui::blue();
        let green = gpui::green();
        let red = gpui::red();
        let colors = vec![
            ("opus".to_string(), blue),
            ("GPT-5".to_string(), green),
            ("gpt 5 mini".to_string(), red),
        ];

        assert_eq!(
            configured_model_color(&colors, &["Claude Opus 5.5", "claude-opus-5-5"]),
            Some(blue)
        );
        assert_eq!(configured_model_color(&colors, &["gpt-5"]), Some(green));
        assert_eq!(
            configured_model_color(&colors, &["GPT-5 Mini", "gpt-5-mini"]),
            Some(red)
        );
        assert_eq!(configured_model_color(&colors, &["gpt-50"]), None);
        assert_eq!(
            configured_model_color(&colors, &["Claude Sonnet 5.5"]),
            None
        );
        assert_eq!(
            configured_model_color(
                &[("opus".to_string(), blue), ("Opus".to_string(), red)],
                &["opus"]
            ),
            Some(blue)
        );
    }

    #[test]
    fn test_effort_heat() {
        let levels = ["low", "medium", "high", "xhigh", "max"];
        assert_eq!(effort_heat(&levels, "low"), None);
        assert_eq!(effort_heat(&levels, "medium"), None);
        assert_eq!(effort_heat(&levels, "high"), Some(0.0));
        assert_eq!(effort_heat(&levels, "xhigh"), Some(0.5));
        assert_eq!(effort_heat(&levels, "max"), Some(1.0));
        assert_eq!(effort_heat(&["low", "high"], "high"), Some(0.0));
        assert_eq!(effort_heat(&["low", "medium"], "medium"), None);
        assert_eq!(effort_heat(&levels, "unknown"), None);
    }
}
