use std::{cmp::Reverse, rc::Rc, sync::Arc};

use acp_thread::AgentSessionConfigOptions;
use agent_client_protocol::schema::v2 as acp;
use agent_servers::AgentServer;

use collections::HashSet;
use fs::Fs;
use fuzzy::StringMatchCandidate;
use gpui::{
    App, BackgroundExecutor, Context, DismissEvent, Entity, Subscription, Task, Window, prelude::*,
};
use ordered_float::OrderedFloat;
use picker::popover_menu::PickerPopoverMenu;
use picker::{Picker, PickerDelegate};
use settings::{AgentConfigOptionValue, SettingsStore};
use ui::{
    ElevationIndex, IconButton, KeyBinding, ListItem, ListItemSpacing, PopoverMenuHandle, Switch,
    SwitchLabelPosition, ToggleState, Tooltip, prelude::*,
};
use unicode_segmentation::UnicodeSegmentation;
use util::ResultExt as _;
use zed_actions::agent::ToggleModelSelector;

use crate::ui::{
    LabelColor, documentation_aside_side, effort_label_color, model_label_color,
    with_width_of_widest,
};
use crate::{
    CycleFavoriteModels, CycleModeSelector, CycleThinkingEffort, ToggleProfileSelector,
    ToggleThinkingEffortMenu,
};

const PICKER_THRESHOLD: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepDirection {
    Increase,
    Decrease,
}

impl StepDirection {
    /// Returns the index to select next among `len` ordered values, or `None` when
    /// already at the end in this direction. An unknown current value starts at the first one.
    pub fn next_index(self, current_index: Option<usize>, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let Some(current_index) = current_index else {
            return Some(0);
        };
        let next_index = match self {
            StepDirection::Increase => current_index + 1,
            StepDirection::Decrease => current_index.checked_sub(1)?,
        };
        (next_index < len).then_some(next_index)
    }
}

pub struct ConfigOptionsView {
    config_options: Rc<dyn AgentSessionConfigOptions>,
    selectors: Vec<Entity<ConfigOptionSelector>>,
    agent_server: Rc<dyn AgentServer>,
    fs: Arc<dyn Fs>,
    config_option_ids: Vec<acp::SessionConfigId>,
    _refresh_task: Task<()>,
}

impl ConfigOptionsView {
    pub fn new(
        config_options: Rc<dyn AgentSessionConfigOptions>,
        agent_server: Rc<dyn AgentServer>,
        fs: Arc<dyn Fs>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selectors = Self::build_selectors(&config_options, &agent_server, &fs, window, cx);
        let config_option_ids = Self::config_option_ids(&config_options);

        let rx = config_options.watch(cx);
        let refresh_task = cx.spawn_in(window, async move |this, cx| {
            if let Some(mut rx) = rx {
                while let Ok(()) = rx.recv().await {
                    this.update_in(cx, |this, window, cx| {
                        this.rebuild_selectors(window, cx);
                        cx.notify();
                    })
                    .log_err();
                }
            }
        });

        Self {
            config_options,
            selectors,
            agent_server,
            fs,
            config_option_ids,
            _refresh_task: refresh_task,
        }
    }

    pub fn toggle_category_picker(
        &mut self,
        category: acp::SessionConfigOptionCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(config_id) = self.first_config_option_id_matching(category, |option| {
            matches!(&option.kind, acp::SessionConfigKind::Select(_))
        }) else {
            return false;
        };

        let Some(selector) = self.selector_for_config_id(&config_id, cx) else {
            return false;
        };

        selector.update(cx, |selector, cx| selector.toggle_picker(window, cx))
    }

    pub fn cycle_category_option(
        &mut self,
        category: acp::SessionConfigOptionCategory,
        favorites_only: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(config_id) = self.first_config_option_id_matching(category, |option| {
            Self::can_cycle_config_option(option, favorites_only)
        }) else {
            return false;
        };

        let Some(next_value) = self.next_value_for_config(&config_id, favorites_only, cx) else {
            return false;
        };
        set_config_option_and_default(
            self.config_options.clone(),
            self.agent_server.clone(),
            config_id,
            next_value,
            self.fs.clone(),
            cx,
        )
        .detach_and_log_err(cx);

        true
    }

    /// Moves the first select option of `category` one value up or down, stopping at
    /// either end instead of wrapping. Returns `false` when no such option exists.
    pub fn step_category_option(
        &mut self,
        category: acp::SessionConfigOptionCategory,
        direction: StepDirection,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(config_id) = self.first_config_option_id_matching(category, |option| {
            matches!(option.kind, acp::SessionConfigKind::Select(_))
        }) else {
            return false;
        };

        let options = extract_options(&self.config_options, &config_id);
        let current_value = get_current_select_value(&self.config_options, &config_id);
        let current_index = current_value
            .as_ref()
            .and_then(|current| options.iter().position(|option| &option.value == current));
        let Some(next_index) = direction.next_index(current_index, options.len()) else {
            return true;
        };
        let Some(next_option) = options.get(next_index) else {
            return true;
        };

        set_config_option_and_default(
            self.config_options.clone(),
            self.agent_server.clone(),
            config_id,
            acp::SessionConfigOptionValue::id(next_option.value.clone()),
            self.fs.clone(),
            cx,
        )
        .detach_and_log_err(cx);

        true
    }

    fn first_config_option_id_matching(
        &self,
        category: acp::SessionConfigOptionCategory,
        predicate: impl Fn(&acp::SessionConfigOption) -> bool,
    ) -> Option<acp::SessionConfigId> {
        self.config_options
            .config_options()
            .into_iter()
            .find(|option| option.category.as_ref() == Some(&category) && predicate(option))
            .map(|option| option.config_id)
    }

    fn can_cycle_config_option(option: &acp::SessionConfigOption, favorites_only: bool) -> bool {
        match &option.kind {
            acp::SessionConfigKind::Select(_) => true,
            acp::SessionConfigKind::Boolean(_) => !favorites_only,
            _ => false,
        }
    }

    fn selector_for_config_id(
        &self,
        config_id: &acp::SessionConfigId,
        cx: &App,
    ) -> Option<Entity<ConfigOptionSelector>> {
        self.selectors
            .iter()
            .find(|selector| selector.read(cx).config_id() == config_id)
            .cloned()
    }

    fn next_value_for_config(
        &self,
        config_id: &acp::SessionConfigId,
        favorites_only: bool,
        cx: &mut Context<Self>,
    ) -> Option<acp::SessionConfigOptionValue> {
        let option = self
            .config_options
            .config_options()
            .into_iter()
            .find(|option| &option.config_id == config_id)?;

        match &option.kind {
            acp::SessionConfigKind::Select(_) => {
                let mut options = extract_options(&self.config_options, config_id);
                if options.is_empty() {
                    return None;
                }

                if favorites_only {
                    let favorites = self
                        .agent_server
                        .favorite_config_option_value_ids(config_id, cx);
                    options.retain(|option| favorites.contains(&option.value));
                    if options.is_empty() {
                        return None;
                    }
                }

                let current_value = get_current_select_value(&self.config_options, config_id);
                let current_index = current_value
                    .as_ref()
                    .and_then(|current| options.iter().position(|option| &option.value == current))
                    .unwrap_or(usize::MAX);

                let next_index = if current_index == usize::MAX {
                    0
                } else {
                    (current_index + 1) % options.len()
                };

                Some(acp::SessionConfigOptionValue::id(
                    options[next_index].value.clone(),
                ))
            }
            acp::SessionConfigKind::Boolean(boolean) => {
                if favorites_only {
                    None
                } else {
                    Some(acp::SessionConfigOptionValue::boolean(
                        !boolean.current_value,
                    ))
                }
            }
            _ => None,
        }
    }

    fn config_option_ids(
        config_options: &Rc<dyn AgentSessionConfigOptions>,
    ) -> Vec<acp::SessionConfigId> {
        config_options
            .config_options()
            .into_iter()
            .map(|option| option.config_id)
            .collect()
    }

    fn rebuild_selectors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Config option updates can mutate option values for existing IDs (for example,
        // reasoning levels after a model switch). Rebuild to refresh cached picker entries.
        self.config_option_ids = Self::config_option_ids(&self.config_options);
        self.selectors = Self::build_selectors(
            &self.config_options,
            &self.agent_server,
            &self.fs,
            window,
            cx,
        );
        cx.notify();
    }

    fn build_selectors(
        config_options: &Rc<dyn AgentSessionConfigOptions>,
        agent_server: &Rc<dyn AgentServer>,
        fs: &Arc<dyn Fs>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Entity<ConfigOptionSelector>> {
        config_options
            .config_options()
            .into_iter()
            .filter(|option| {
                matches!(
                    &option.kind,
                    acp::SessionConfigKind::Select(_) | acp::SessionConfigKind::Boolean(_)
                )
            })
            .map(|option| {
                let config_options = config_options.clone();
                let agent_server = agent_server.clone();
                let fs = fs.clone();
                cx.new(|cx| {
                    ConfigOptionSelector::new(
                        config_options,
                        option.config_id,
                        agent_server,
                        fs,
                        window,
                        cx,
                    )
                })
            })
            .collect()
    }
}

impl Render for ConfigOptionsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.selectors.is_empty() {
            return div().into_any_element();
        }

        // Icon toggles lead so they sit next to the context usage indicator rather than
        // between the labeled selectors.
        let (icon_toggles, labeled_selectors): (Vec<_>, Vec<_>) = self
            .selectors
            .iter()
            .cloned()
            .partition(|selector| selector.read(cx).renders_as_icon());

        h_flex()
            .min_w_0()
            .flex_wrap()
            .gap_1()
            .children(icon_toggles)
            .children(labeled_selectors)
            .into_any_element()
    }
}

/// Boolean options that agents commonly expose, shown as a compact icon toggle instead of a
/// labeled switch so they match the equivalent controls of the native agent.
struct BooleanOptionIcons {
    enabled: IconName,
    disabled: IconName,
}

impl BooleanOptionIcons {
    fn for_config_id(config_id: &acp::SessionConfigId) -> Option<Self> {
        match config_id.0.as_ref() {
            "fast" => Some(Self {
                enabled: IconName::FastForward,
                disabled: IconName::FastForwardOff,
            }),
            _ => None,
        }
    }
}

struct ConfigOptionSelector {
    config_options: Rc<dyn AgentSessionConfigOptions>,
    config_id: acp::SessionConfigId,
    agent_server: Rc<dyn AgentServer>,
    fs: Arc<dyn Fs>,
    picker_handle: Option<PopoverMenuHandle<Picker<ConfigOptionPickerDelegate>>>,
    picker: Option<Entity<Picker<ConfigOptionPickerDelegate>>>,
    setting_value: bool,
}

impl ConfigOptionSelector {
    pub fn new(
        config_options: Rc<dyn AgentSessionConfigOptions>,
        config_id: acp::SessionConfigId,
        agent_server: Rc<dyn AgentServer>,
        fs: Arc<dyn Fs>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current_option = config_options
            .config_options()
            .into_iter()
            .find(|opt| opt.config_id == config_id);
        let option_count = current_option
            .as_ref()
            .map(count_config_options)
            .unwrap_or(0);
        let is_select = current_option
            .as_ref()
            .is_some_and(|option| matches!(&option.kind, acp::SessionConfigKind::Select(_)));

        let is_searchable = option_count >= PICKER_THRESHOLD;

        let (picker_handle, picker) = if is_select {
            let config_options = config_options.clone();
            let config_id = config_id.clone();
            let agent_server = agent_server.clone();
            let fs = fs.clone();
            let picker = cx.new(move |picker_cx| {
                let delegate = ConfigOptionPickerDelegate::new(
                    config_options,
                    config_id,
                    agent_server,
                    fs,
                    window,
                    picker_cx,
                );

                if is_searchable {
                    Picker::list(delegate, window, picker_cx)
                } else {
                    Picker::nonsearchable_list(delegate, window, picker_cx)
                }
                .show_scrollbar(true)
                .initial_width(rems(20.))
            });
            (Some(PopoverMenuHandle::default()), Some(picker))
        } else {
            (None, None)
        };

        Self {
            config_options,
            config_id,
            agent_server,
            fs,
            picker_handle,
            picker,
            setting_value: false,
        }
    }

    fn current_option(&self) -> Option<acp::SessionConfigOption> {
        self.config_options
            .config_options()
            .into_iter()
            .find(|opt| opt.config_id == self.config_id)
    }

    fn config_id(&self) -> &acp::SessionConfigId {
        &self.config_id
    }

    fn renders_as_icon(&self) -> bool {
        BooleanOptionIcons::for_config_id(&self.config_id).is_some()
            && self
                .current_option()
                .is_some_and(|option| matches!(option.kind, acp::SessionConfigKind::Boolean(_)))
    }

    fn toggle_picker(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self
            .current_option()
            .is_some_and(|option| matches!(option.kind, acp::SessionConfigKind::Select(_)))
        {
            return false;
        }

        if let Some(picker_handle) = &self.picker_handle {
            picker_handle.toggle(window, cx);
            true
        } else {
            false
        }
    }

    fn current_value_name(&self) -> String {
        let Some(option) = self.current_option() else {
            return "Unknown".to_string();
        };

        match &option.kind {
            acp::SessionConfigKind::Select(select) => {
                find_option_name(&select.options, &select.current_value)
                    .unwrap_or_else(|| "Unknown".to_string())
            }
            _ => "Unknown".to_string(),
        }
    }

    fn current_value_color(
        &self,
        option: &acp::SessionConfigOption,
        cx: &App,
    ) -> Option<LabelColor> {
        let acp::SessionConfigKind::Select(select) = &option.kind else {
            return None;
        };
        match option.category.as_ref()? {
            acp::SessionConfigOptionCategory::ThoughtLevel => {
                let options = extract_options(&self.config_options, &self.config_id);
                let level_values = options
                    .iter()
                    .map(|option| option.value.0.as_ref())
                    .collect::<Vec<_>>();
                effort_label_color(
                    &level_values,
                    &select.current_value.0,
                    &self.current_value_name(),
                    cx,
                )
            }
            acp::SessionConfigOptionCategory::Model => {
                model_label_color(&[&self.current_value_name(), &select.current_value.0], cx)
            }
            _ => None,
        }
    }

    fn handles_category_keybindings(&self, category: &acp::SessionConfigOptionCategory) -> bool {
        self.config_options
            .config_options()
            .into_iter()
            .find(|option| {
                option.category.as_ref() == Some(category)
                    && matches!(&option.kind, acp::SessionConfigKind::Select(_))
            })
            .is_some_and(|option| option.config_id == self.config_id)
    }

    fn render_trigger_button(&self, _window: &mut Window, cx: &mut Context<Self>) -> Button {
        let Some(option) = self.current_option() else {
            return Button::new("config-option-trigger", "Unknown")
                .label_size(LabelSize::Small)
                .color(Color::Muted)
                .disabled(true);
        };

        let picker_deployed = self
            .picker_handle
            .as_ref()
            .is_some_and(|picker_handle| picker_handle.is_deployed());
        let icon = if picker_deployed {
            IconName::ChevronUp
        } else {
            IconName::ChevronDown
        };

        let colored_label = self.current_value_color(&option, cx);

        Button::new(
            ElementId::Name(format!("config-option-{}", option.config_id.0).into()),
            truncated_display_name(&self.current_value_name()),
        )
        .label_size(LabelSize::Small)
        .color(colored_label.map_or(Color::Muted, |label| label.label_color(cx)))
        .when_some(colored_label, |this, label| {
            this.style(label.button_style())
        })
        .end_icon(
            Icon::new(icon).size(IconSize::XSmall).color(
                colored_label.map_or(Color::Muted, |label| label.icon_color(Color::Muted, cx)),
            ),
        )
        .when(has_fixed_width(&option), |button| {
            button.width(relative(1.))
        })
        .disabled(self.setting_value)
    }

    fn render_width_sizers(&self) -> Vec<Button> {
        extract_options(&self.config_options, &self.config_id)
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                Button::new(
                    ("config-option-sizer", index),
                    truncated_display_name(&value.name),
                )
                .label_size(LabelSize::Small)
                .end_icon(Icon::new(IconName::ChevronDown).size(IconSize::XSmall))
            })
            .collect()
    }
}

/// The thinking effort selector keeps the width of its widest value, so that changing the
/// effort doesn't shift the controls next to it.
fn has_fixed_width(option: &acp::SessionConfigOption) -> bool {
    option.category == Some(acp::SessionConfigOptionCategory::ThoughtLevel)
}

fn truncated_display_name(name: &str) -> String {
    let mut graphemes = name.graphemes(true);
    let truncated = graphemes.by_ref().take(32).collect::<String>();
    if graphemes.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

impl Render for ConfigOptionSelector {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(option) = self.current_option() else {
            return div().into_any_element();
        };

        match &option.kind {
            acp::SessionConfigKind::Select(_) => {
                let (Some(picker), Some(picker_handle)) =
                    (self.picker.clone(), self.picker_handle.clone())
                else {
                    return div().into_any_element();
                };

                let trigger_button = self.render_trigger_button(window, cx);

                let show_category_keybindings = option
                    .category
                    .as_ref()
                    .is_some_and(|category| self.handles_category_keybindings(category));
                let option_category = option.category.clone();
                let option_name = option.name.clone();
                let option_description: Option<SharedString> =
                    option.description.clone().map(Into::into);

                let tooltip = Tooltip::element(move |_window, cx| {
                    let mut content = v_flex().gap_1().child(Label::new(option_name.clone()));
                    if let Some(desc) = option_description.as_ref() {
                        content = content.child(
                            Label::new(desc.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        );
                    }

                    let action_tooltip_container = |label: &str, keybinding: KeyBinding| {
                        h_flex()
                            .pt_1()
                            .gap_2()
                            .justify_between()
                            .border_t_1()
                            .border_color(cx.theme().colors().border_variant)
                            .child(Label::new(label))
                            .child(keybinding)
                    };

                    if show_category_keybindings && let Some(category) = &option_category {
                        match category {
                            acp::SessionConfigOptionCategory::Mode => {
                                content = content
                                    .child(action_tooltip_container(
                                        "Change Mode",
                                        KeyBinding::for_action(&ToggleProfileSelector, cx),
                                    ))
                                    .child(action_tooltip_container(
                                        "Cycle Through Modes",
                                        KeyBinding::for_action(&CycleModeSelector, cx),
                                    ));
                            }
                            acp::SessionConfigOptionCategory::Model => {
                                content = content
                                    .child(action_tooltip_container(
                                        "Change Model",
                                        KeyBinding::for_action(&ToggleModelSelector, cx),
                                    ))
                                    .child(action_tooltip_container(
                                        "Cycle Favorite Models",
                                        KeyBinding::for_action(&CycleFavoriteModels, cx),
                                    ));
                            }
                            acp::SessionConfigOptionCategory::ThoughtLevel => {
                                content = content
                                    .child(action_tooltip_container(
                                        "Change Thinking Effort",
                                        KeyBinding::for_action(&ToggleThinkingEffortMenu, cx),
                                    ))
                                    .child(action_tooltip_container(
                                        "Cycle Thinking Effort",
                                        KeyBinding::for_action(&CycleThinkingEffort, cx),
                                    ));
                            }
                            _ => {}
                        }
                    }
                    content.into_any()
                });

                let popover = PickerPopoverMenu::new(
                    picker,
                    trigger_button,
                    tooltip,
                    gpui::Anchor::BottomRight,
                    cx,
                )
                .with_handle(picker_handle)
                .render(window, cx);

                if has_fixed_width(&option) {
                    with_width_of_widest(popover, self.render_width_sizers()).into_any_element()
                } else {
                    popover.into_any_element()
                }
            }
            acp::SessionConfigKind::Boolean(boolean) => {
                let option_id = option.config_id.clone();
                let option_name: SharedString = option.name.clone().into();
                let option_description: Option<SharedString> =
                    option.description.clone().map(Into::into);
                let tooltip_name = option_name.clone();
                let tooltip = Tooltip::element(move |_window, _cx| {
                    let mut content = v_flex().gap_1().child(Label::new(tooltip_name.clone()));
                    if let Some(desc) = option_description.as_ref() {
                        content = content.child(
                            Label::new(desc.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        );
                    }
                    content.into_any()
                });

                let config_id = self.config_id.clone();
                let config_options = self.config_options.clone();
                let agent_server = self.agent_server.clone();
                let fs = self.fs.clone();
                let set_value = move |next_value: bool, cx: &mut App| {
                    if !config_options.config_options().iter().any(|option| {
                        option.config_id == config_id
                            && matches!(&option.kind, acp::SessionConfigKind::Boolean(_))
                    }) {
                        return;
                    }

                    set_config_option_and_default(
                        config_options.clone(),
                        agent_server.clone(),
                        config_id.clone(),
                        acp::SessionConfigOptionValue::boolean(next_value),
                        fs.clone(),
                        cx,
                    )
                    .detach_and_log_err(cx);
                };
                let current_value = boolean.current_value;

                if let Some(icons) = BooleanOptionIcons::for_config_id(&option_id) {
                    let (icon, icon_color) = if current_value {
                        (icons.enabled, Color::Accent)
                    } else {
                        (
                            icons.disabled,
                            Color::Custom(cx.theme().colors().icon_disabled.opacity(0.8)),
                        )
                    };
                    return IconButton::new(
                        ElementId::Name(format!("config-option-{}", option_id.0).into()),
                        icon,
                    )
                    .icon_size(IconSize::Small)
                    .icon_color(icon_color)
                    .disabled(self.setting_value)
                    .tooltip(tooltip)
                    .on_click(move |_, _window, cx| set_value(!current_value, cx))
                    .into_any_element();
                }

                let toggle_state = if current_value {
                    ToggleState::Selected
                } else {
                    ToggleState::Unselected
                };

                h_flex()
                    .id(ElementId::Name(
                        format!("config-option-{}", option_id.0).into(),
                    ))
                    .pr_1()
                    .tooltip(tooltip)
                    .child(
                        Switch::new(
                            ElementId::Name(format!("config-option-{}-switch", option_id.0).into()),
                            toggle_state,
                        )
                        .label(option_name)
                        .label_position(SwitchLabelPosition::Start)
                        .label_size(LabelSize::Small)
                        .label_color(Color::Muted)
                        .disabled(self.setting_value)
                        .on_click(move |state, _window, cx| {
                            set_value(matches!(state, ToggleState::Selected), cx)
                        }),
                    )
                    .into_any_element()
            }
            _ => div().into_any_element(),
        }
    }
}

#[derive(Clone)]
enum ConfigOptionPickerEntry {
    Separator(SharedString),
    Option(ConfigOptionValue),
}

#[derive(Clone)]
struct ConfigOptionValue {
    value: acp::SessionConfigValueId,
    name: String,
    description: Option<String>,
    group: Option<String>,
}

struct ConfigOptionPickerDelegate {
    config_options: Rc<dyn AgentSessionConfigOptions>,
    config_id: acp::SessionConfigId,
    agent_server: Rc<dyn AgentServer>,
    fs: Arc<dyn Fs>,
    filtered_entries: Vec<ConfigOptionPickerEntry>,
    all_options: Vec<ConfigOptionValue>,
    selected_index: usize,
    selected_description: Option<(usize, SharedString)>,
    favorites: HashSet<acp::SessionConfigValueId>,
    _settings_subscription: Subscription,
}

impl ConfigOptionPickerDelegate {
    fn new(
        config_options: Rc<dyn AgentSessionConfigOptions>,
        config_id: acp::SessionConfigId,
        agent_server: Rc<dyn AgentServer>,
        fs: Arc<dyn Fs>,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Self {
        let favorites = agent_server.favorite_config_option_value_ids(&config_id, cx);

        let all_options = extract_options(&config_options, &config_id);
        let filtered_entries = options_to_picker_entries(&all_options, &favorites);

        let current_value = get_current_select_value(&config_options, &config_id);
        let selected_index = current_value
            .and_then(|current| {
                filtered_entries.iter().position(|entry| {
                    matches!(entry, ConfigOptionPickerEntry::Option(opt) if opt.value == current)
                })
            })
            .unwrap_or(0);

        let agent_server_for_subscription = agent_server.clone();
        let config_id_for_subscription = config_id.clone();
        let settings_subscription =
            cx.observe_global_in::<SettingsStore>(window, move |picker, window, cx| {
                let new_favorites = agent_server_for_subscription
                    .favorite_config_option_value_ids(&config_id_for_subscription, cx);
                if new_favorites != picker.delegate.favorites {
                    picker.delegate.favorites = new_favorites;
                    picker.refresh(window, cx);
                }
            });

        cx.notify();

        Self {
            config_options,
            config_id,
            agent_server,
            fs,
            filtered_entries,
            all_options,
            selected_index,
            selected_description: None,
            favorites,
            _settings_subscription: settings_subscription,
        }
    }

    fn current_value(&self) -> Option<acp::SessionConfigValueId> {
        get_current_select_value(&self.config_options, &self.config_id)
    }
}

impl PickerDelegate for ConfigOptionPickerDelegate {
    type ListItem = AnyElement;

    fn name() -> &'static str {
        "config options"
    }

    fn match_count(&self) -> usize {
        self.filtered_entries.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.selected_index = ix.min(self.filtered_entries.len().saturating_sub(1));
        cx.notify();
    }

    fn can_select(&self, ix: usize, _window: &mut Window, _cx: &mut Context<Picker<Self>>) -> bool {
        match self.filtered_entries.get(ix) {
            Some(ConfigOptionPickerEntry::Option(option)) => {
                extract_options(&self.config_options, &self.config_id)
                    .iter()
                    .any(|current| current.value == option.value)
            }
            Some(ConfigOptionPickerEntry::Separator(_)) | None => false,
        }
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Select an option…".into()
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let all_options = self.all_options.clone();

        cx.spawn_in(window, async move |this, cx| {
            let filtered_options = match this
                .read_with(cx, |_, cx| {
                    if query.is_empty() {
                        None
                    } else {
                        Some((all_options.clone(), query.clone(), cx.background_executor().clone()))
                    }
                })
                .ok()
                .flatten()
            {
                Some((options, q, executor)) => fuzzy_search_options(options, &q, executor).await,
                None => all_options,
            };

            this.update_in(cx, |this, window, cx| {
                this.delegate.filtered_entries =
                    options_to_picker_entries(&filtered_options, &this.delegate.favorites);

                let current_value = this.delegate.current_value();
                let new_index = current_value
                    .and_then(|current| {
                        this.delegate.filtered_entries.iter().position(|entry| {
                            matches!(entry, ConfigOptionPickerEntry::Option(opt) if opt.value == current)
                        })
                    })
                    .unwrap_or(0);

                this.set_selected_index(new_index, Some(picker::Direction::Down), true, window, cx);
                cx.notify();
            })
            .ok();
        })
    }

    fn confirm(&mut self, _secondary: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let Some(ConfigOptionPickerEntry::Option(option)) =
            self.filtered_entries.get(self.selected_index)
        {
            // A deployed picker may outlive the option kind or choices that created it.
            if !extract_options(&self.config_options, &self.config_id)
                .iter()
                .any(|current| current.value == option.value)
            {
                return;
            }

            set_config_option_and_default(
                self.config_options.clone(),
                self.agent_server.clone(),
                self.config_id.clone(),
                acp::SessionConfigOptionValue::id(option.value.clone()),
                self.fs.clone(),
                cx,
            )
            .detach_and_log_err(cx);

            cx.emit(DismissEvent);
        }
    }

    fn dismissed(&mut self, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        cx.defer_in(window, |picker, window, cx| {
            picker.set_query("", window, cx);
        });
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        match self.filtered_entries.get(ix)? {
            ConfigOptionPickerEntry::Separator(title) => Some(
                div()
                    .when(ix > 0, |this| this.mt_1())
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(cx.theme().colors().text_muted)
                            .child(title.clone()),
                    )
                    .into_any_element(),
            ),
            ConfigOptionPickerEntry::Option(option) => {
                let current_value = self.current_value();
                let is_selected = current_value.as_ref() == Some(&option.value);

                let is_favorite = self.favorites.contains(&option.value);

                let option_name = option.name.clone();
                let description = option.description.clone();

                Some(
                    div()
                        .id(("config-option-picker-item", ix))
                        .when_some(description, |this, desc| {
                            let desc: SharedString = desc.into();
                            this.on_hover(cx.listener(move |menu, hovered, _, cx| {
                                if *hovered {
                                    menu.delegate.selected_description = Some((ix, desc.clone()));
                                } else if matches!(menu.delegate.selected_description, Some((id, _)) if id == ix)
                                {
                                    menu.delegate.selected_description = None;
                                }
                                cx.notify();
                            }))
                        })
                        .child(
                            ListItem::new(ix)
                                .inset(true)
                                .spacing(ListItemSpacing::Sparse)
                                .toggle_state(selected)
                                .child(h_flex().w_full().child(Label::new(option_name).truncate()))
                                .end_slot(div().pr_2().when(is_selected, |this| {
                                    this.child(Icon::new(IconName::Check).color(Color::Accent))
                                }))
                                .end_slot_on_hover(div().pr_1p5().child({
                                    let (icon, color, tooltip) = if is_favorite {
                                        (IconName::StarFilled, Color::Accent, "Unfavorite")
                                    } else {
                                        (IconName::Star, Color::Default, "Favorite")
                                    };

                                    let config_id = self.config_id.clone();
                                    let value_id = option.value.clone();
                                    let agent_server = self.agent_server.clone();
                                    let fs = self.fs.clone();
                                    let config_options = self.config_options.clone();

                                    IconButton::new(("toggle-favorite-config-option", ix), icon)
                                        .layer(ElevationIndex::ElevatedSurface)
                                        .icon_color(color)
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text(tooltip))
                                        .on_click(move |_, _, cx| {
                                            if !extract_options(&config_options, &config_id)
                                                .iter()
                                                .any(|current| current.value == value_id)
                                            {
                                                return;
                                            }

                                            agent_server.toggle_favorite_config_option_value(
                                                config_id.clone(),
                                                value_id.clone(),
                                                !is_favorite,
                                                fs.clone(),
                                                cx,
                                            );
                                        })
                                })),
                        )
                        .into_any_element(),
                )
            }
        }
    }

    fn documentation_aside(
        &self,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<ui::DocumentationAside> {
        self.selected_description.as_ref().map(|(_, description)| {
            let description = description.clone();

            let side = documentation_aside_side(cx);

            ui::DocumentationAside::new(
                side,
                Rc::new(move |_| Label::new(description.clone()).into_any_element()),
            )
        })
    }

    fn documentation_aside_index(&self) -> Option<usize> {
        self.selected_description.as_ref().map(|(ix, _)| *ix)
    }
}

fn extract_options(
    config_options: &Rc<dyn AgentSessionConfigOptions>,
    config_id: &acp::SessionConfigId,
) -> Vec<ConfigOptionValue> {
    let Some(option) = config_options
        .config_options()
        .into_iter()
        .find(|opt| &opt.config_id == config_id)
    else {
        return Vec::new();
    };

    match &option.kind {
        acp::SessionConfigKind::Select(select) => match &select.options {
            acp::SessionConfigSelectOptions::Ungrouped(options) => options
                .iter()
                .map(|opt| ConfigOptionValue {
                    value: opt.value.clone(),
                    name: opt.name.clone(),
                    description: opt.description.clone(),
                    group: None,
                })
                .collect(),
            acp::SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|group| {
                    group.options.iter().map(|opt| ConfigOptionValue {
                        value: opt.value.clone(),
                        name: opt.name.clone(),
                        description: opt.description.clone(),
                        group: Some(group.name.clone()),
                    })
                })
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn get_current_select_value(
    config_options: &Rc<dyn AgentSessionConfigOptions>,
    config_id: &acp::SessionConfigId,
) -> Option<acp::SessionConfigValueId> {
    config_options
        .config_options()
        .into_iter()
        .find(|opt| &opt.config_id == config_id)
        .and_then(|opt| match &opt.kind {
            acp::SessionConfigKind::Select(select) => Some(select.current_value.clone()),
            _ => None,
        })
}

fn set_config_option_and_default(
    config_options: Rc<dyn AgentSessionConfigOptions>,
    agent_server: Rc<dyn AgentServer>,
    config_id: acp::SessionConfigId,
    value: acp::SessionConfigOptionValue,
    fs: Arc<dyn Fs>,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    let response = config_options.set_config_option(config_id.clone(), value, cx);
    cx.spawn(async move |cx| {
        let options = response.await?;
        // Saving the request would make rejected or normalized choices future defaults.
        if let Some(value) = options
            .iter()
            .find(|option| option.config_id == config_id)
            .and_then(setting_value_for_config_option)
        {
            cx.update(|cx| {
                agent_server.set_default_config_option(config_id.0.as_ref(), Some(value), fs, cx);
            });
        }
        Ok(())
    })
}

fn setting_value_for_config_option(
    option: &acp::SessionConfigOption,
) -> Option<AgentConfigOptionValue> {
    match &option.kind {
        acp::SessionConfigKind::Select(select)
            if find_option_name(&select.options, &select.current_value).is_some() =>
        {
            Some(AgentConfigOptionValue::ValueId(
                select.current_value.0.to_string(),
            ))
        }
        acp::SessionConfigKind::Boolean(boolean) => {
            Some(AgentConfigOptionValue::Boolean(boolean.current_value))
        }
        _ => None,
    }
}

fn options_to_picker_entries(
    options: &[ConfigOptionValue],
    favorites: &HashSet<acp::SessionConfigValueId>,
) -> Vec<ConfigOptionPickerEntry> {
    let mut entries = Vec::new();

    let mut favorite_options = Vec::new();

    for option in options {
        if favorites.contains(&option.value) {
            favorite_options.push(option.clone());
        }
    }

    if !favorite_options.is_empty() {
        entries.push(ConfigOptionPickerEntry::Separator("Favorites".into()));
        for option in favorite_options {
            entries.push(ConfigOptionPickerEntry::Option(option));
        }

        // If the remaining list would start ungrouped (group == None), insert a separator so
        // Favorites doesn't visually run into the main list.
        if let Some(option) = options.first()
            && option.group.is_none()
        {
            entries.push(ConfigOptionPickerEntry::Separator("All Options".into()));
        }
    }

    let mut current_group: Option<String> = None;
    for option in options {
        if option.group != current_group {
            if let Some(group_name) = &option.group {
                entries.push(ConfigOptionPickerEntry::Separator(
                    group_name.clone().into(),
                ));
            }
            current_group = option.group.clone();
        }
        entries.push(ConfigOptionPickerEntry::Option(option.clone()));
    }

    entries
}

async fn fuzzy_search_options(
    options: Vec<ConfigOptionValue>,
    query: &str,
    executor: BackgroundExecutor,
) -> Vec<ConfigOptionValue> {
    let candidates = options
        .iter()
        .enumerate()
        .map(|(ix, opt)| StringMatchCandidate::new(ix, &opt.name))
        .collect::<Vec<_>>();

    let mut matches = fuzzy::match_strings(
        &candidates,
        query,
        false,
        true,
        100,
        &Default::default(),
        executor,
    )
    .await;

    matches.sort_unstable_by_key(|mat| {
        let candidate = &candidates[mat.candidate_id];
        (Reverse(OrderedFloat(mat.score)), candidate.id)
    });

    matches
        .into_iter()
        .map(|mat| options[mat.candidate_id].clone())
        .collect()
}

fn find_option_name(
    options: &acp::SessionConfigSelectOptions,
    value_id: &acp::SessionConfigValueId,
) -> Option<String> {
    match options {
        acp::SessionConfigSelectOptions::Ungrouped(opts) => opts
            .iter()
            .find(|o| &o.value == value_id)
            .map(|o| o.name.clone()),
        acp::SessionConfigSelectOptions::Grouped(groups) => groups.iter().find_map(|group| {
            group
                .options
                .iter()
                .find(|o| &o.value == value_id)
                .map(|o| o.name.clone())
        }),
        _ => None,
    }
}

fn count_config_options(option: &acp::SessionConfigOption) -> usize {
    match &option.kind {
        acp::SessionConfigKind::Select(select) => match &select.options {
            acp::SessionConfigSelectOptions::Ungrouped(options) => options.len(),
            acp::SessionConfigSelectOptions::Grouped(groups) => {
                groups.iter().map(|g| g.options.len()).sum()
            }
            _ => 0,
        },
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acp_thread::AgentConnection;
    use fs::FakeFs;
    use gpui::TestAppContext;
    use parking_lot::Mutex;
    use project::{AgentId, Project};
    use std::{any::Any, cell::RefCell};

    #[test]
    fn test_step_direction_stops_at_ends() {
        use StepDirection::{Decrease, Increase};

        assert_eq!(Increase.next_index(Some(0), 3), Some(1));
        assert_eq!(Increase.next_index(Some(2), 3), None);
        assert_eq!(Decrease.next_index(Some(2), 3), Some(1));
        assert_eq!(Decrease.next_index(Some(0), 3), None);
        assert_eq!(Increase.next_index(None, 3), Some(0));
        assert_eq!(Decrease.next_index(None, 3), Some(0));
        assert_eq!(Increase.next_index(None, 0), None);
    }

    #[gpui::test]
    fn config_defaults_wait_for_authoritative_response(cx: &mut TestAppContext) {
        for boolean in [false, true] {
            let agent_server = Rc::new(TestAgentServer::default());
            let offered = if boolean {
                acp::SessionConfigOption::boolean("config", "Config", false)
            } else {
                acp::SessionConfigOption::select(
                    "config",
                    "Config",
                    "auto",
                    vec![
                        acp::SessionConfigSelectOption::new("auto", "Auto"),
                        acp::SessionConfigSelectOption::new("manual", "Manual"),
                    ],
                )
            }
            .category(acp::SessionConfigOptionCategory::ModelConfig);
            let config_options = Rc::new(TestSessionConfigOptions::new(vec![offered.clone()]));
            let _updates = config_options.updates.borrow().receiver();
            let fs: Arc<dyn Fs> = FakeFs::new(cx.executor());
            let view = cx.update(|cx| {
                let config_options: Rc<dyn AgentSessionConfigOptions> = config_options.clone();
                let agent_server: Rc<dyn AgentServer> = agent_server.clone();
                cx.new(|_| ConfigOptionsView {
                    config_option_ids: ConfigOptionsView::config_option_ids(&config_options),
                    config_options,
                    selectors: Vec::new(),
                    agent_server,
                    fs,
                    _refresh_task: Task::ready(()),
                })
            });
            let rejected = config_options.defer_next_response();
            view.update(cx, |view, cx| {
                assert!(view.cycle_category_option(
                    acp::SessionConfigOptionCategory::ModelConfig,
                    false,
                    cx
                ));
            });
            assert!(
                agent_server.saved_defaults.lock().is_empty(),
                "do not persist before acceptance"
            );
            rejected
                .send(Err(anyhow::anyhow!("rejected")))
                .expect("config response");
            cx.run_until_parked();
            assert!(
                agent_server.saved_defaults.lock().is_empty(),
                "rejection preserves preferences"
            );

            let accepted = config_options.defer_next_response();
            view.update(cx, |view, cx| {
                assert!(view.cycle_category_option(
                    acp::SessionConfigOptionCategory::ModelConfig,
                    false,
                    cx
                ));
            });
            config_options.replace_options(vec![offered.clone()]);
            accepted
                .send(Ok(vec![offered]))
                .expect("authoritative response");
            cx.run_until_parked();
            let expected = if boolean {
                AgentConfigOptionValue::Boolean(false)
            } else {
                AgentConfigOptionValue::ValueId("auto".into())
            };
            assert_eq!(
                agent_server.saved_defaults.lock().as_slice(),
                &[("config".into(), Some(expected))]
            );

            let removed = config_options.defer_next_response();
            view.update(cx, |view, cx| {
                assert!(view.cycle_category_option(
                    acp::SessionConfigOptionCategory::ModelConfig,
                    false,
                    cx
                ));
            });
            config_options.replace_options(Vec::new());
            removed.send(Ok(Vec::new())).expect("option removed");
            cx.run_until_parked();
            assert_eq!(
                agent_server.saved_defaults.lock().len(),
                1,
                "do not save an absent option"
            );
        }
    }

    #[gpui::test]
    fn cycling_config_option_saves_selected_value_as_default(cx: &mut TestAppContext) {
        let agent_server = Rc::new(TestAgentServer::default());
        let config_options = Rc::new(TestSessionConfigOptions::new(vec![
            acp::SessionConfigOption::select(
                "mode",
                "Mode",
                "auto",
                vec![
                    acp::SessionConfigSelectOption::new("auto", "Auto"),
                    acp::SessionConfigSelectOption::new("manual", "Manual"),
                ],
            )
            .category(acp::SessionConfigOptionCategory::Mode),
        ]));
        let fs: Arc<dyn Fs> = FakeFs::new(cx.executor());

        cx.update(|cx| {
            let config_options: Rc<dyn AgentSessionConfigOptions> = config_options.clone();
            let agent_server: Rc<dyn AgentServer> = agent_server.clone();
            let fs = fs.clone();
            let view = cx.new(|_| ConfigOptionsView {
                config_option_ids: ConfigOptionsView::config_option_ids(&config_options),
                config_options,
                selectors: Vec::new(),
                agent_server,
                fs,
                _refresh_task: Task::ready(()),
            });

            assert!(view.update(cx, |view, cx| {
                view.cycle_category_option(acp::SessionConfigOptionCategory::Mode, false, cx)
            }));
        });
        cx.run_until_parked();

        assert_eq!(
            agent_server.saved_defaults.lock().as_slice(),
            &[(
                "mode".to_string(),
                Some(AgentConfigOptionValue::ValueId("manual".to_string()))
            )]
        );
        assert_eq!(
            config_options.set_values.borrow().as_slice(),
            &[(
                "mode".to_string(),
                acp::SessionConfigOptionValue::id("manual")
            )]
        );
    }

    #[gpui::test]
    fn only_known_boolean_options_render_as_icons(cx: &mut TestAppContext) {
        let agent_server: Rc<dyn AgentServer> = Rc::new(TestAgentServer::default());
        let config_options: Rc<dyn AgentSessionConfigOptions> =
            Rc::new(TestSessionConfigOptions::new(vec![
                acp::SessionConfigOption::boolean("web_search", "Web Search", false)
                    .category(acp::SessionConfigOptionCategory::ModelConfig),
                acp::SessionConfigOption::boolean("fast", "Fast mode", false)
                    .category(acp::SessionConfigOptionCategory::ModelConfig),
            ]));
        let fs: Arc<dyn Fs> = FakeFs::new(cx.executor());
        let cx = cx.add_empty_window();

        let renders_as_icon = cx.update(|window, cx| {
            ["web_search", "fast"].map(|config_id| {
                let selector = cx.new(|cx| {
                    ConfigOptionSelector::new(
                        config_options.clone(),
                        config_id.into(),
                        agent_server.clone(),
                        fs.clone(),
                        window,
                        cx,
                    )
                });
                selector.read(cx).renders_as_icon()
            })
        });

        assert_eq!(renders_as_icon, [false, true]);
    }

    #[gpui::test]
    fn cycling_boolean_config_option_saves_selected_value_as_default(cx: &mut TestAppContext) {
        let agent_server = Rc::new(TestAgentServer::default());
        let config_options = Rc::new(TestSessionConfigOptions::new(vec![
            acp::SessionConfigOption::boolean("web_search", "Web Search", false)
                .category(acp::SessionConfigOptionCategory::ModelConfig),
        ]));
        let fs: Arc<dyn Fs> = FakeFs::new(cx.executor());

        cx.update(|cx| {
            let config_options: Rc<dyn AgentSessionConfigOptions> = config_options.clone();
            let agent_server: Rc<dyn AgentServer> = agent_server.clone();
            let fs = fs.clone();
            let view = cx.new(|_| ConfigOptionsView {
                config_option_ids: ConfigOptionsView::config_option_ids(&config_options),
                config_options,
                selectors: Vec::new(),
                agent_server,
                fs,
                _refresh_task: Task::ready(()),
            });

            assert!(view.update(cx, |view, cx| {
                view.cycle_category_option(acp::SessionConfigOptionCategory::ModelConfig, false, cx)
            }));
        });
        cx.run_until_parked();

        assert_eq!(
            agent_server.saved_defaults.lock().as_slice(),
            &[(
                "web_search".to_string(),
                Some(AgentConfigOptionValue::Boolean(true))
            )]
        );
        assert_eq!(
            config_options.set_values.borrow().as_slice(),
            &[(
                "web_search".to_string(),
                acp::SessionConfigOptionValue::boolean(true)
            )]
        );
    }

    #[gpui::test]
    fn cycling_category_cycles_boolean_config_option_first(cx: &mut TestAppContext) {
        let agent_server = Rc::new(TestAgentServer::default());
        let config_options = Rc::new(TestSessionConfigOptions::new(vec![
            acp::SessionConfigOption::boolean("web_search", "Web Search", false)
                .category(acp::SessionConfigOptionCategory::Model),
            acp::SessionConfigOption::select(
                "model",
                "Model",
                "small",
                vec![
                    acp::SessionConfigSelectOption::new("small", "Small"),
                    acp::SessionConfigSelectOption::new("large", "Large"),
                ],
            )
            .category(acp::SessionConfigOptionCategory::Model),
        ]));
        let fs: Arc<dyn Fs> = FakeFs::new(cx.executor());

        cx.update(|cx| {
            let config_options: Rc<dyn AgentSessionConfigOptions> = config_options.clone();
            let agent_server: Rc<dyn AgentServer> = agent_server.clone();
            let fs = fs.clone();
            let view = cx.new(|_| ConfigOptionsView {
                config_option_ids: ConfigOptionsView::config_option_ids(&config_options),
                config_options,
                selectors: Vec::new(),
                agent_server,
                fs,
                _refresh_task: Task::ready(()),
            });

            assert!(view.update(cx, |view, cx| {
                view.cycle_category_option(acp::SessionConfigOptionCategory::Model, false, cx)
            }));
        });
        cx.run_until_parked();

        assert_eq!(
            agent_server.saved_defaults.lock().as_slice(),
            &[(
                "web_search".to_string(),
                Some(AgentConfigOptionValue::Boolean(true))
            )]
        );
        assert_eq!(
            config_options.set_values.borrow().as_slice(),
            &[(
                "web_search".to_string(),
                acp::SessionConfigOptionValue::boolean(true)
            )]
        );
    }

    #[gpui::test]
    fn toggling_category_picker_without_select_config_option_is_unhandled(cx: &mut TestAppContext) {
        let agent_server = Rc::new(TestAgentServer::default());
        let config_options = Rc::new(TestSessionConfigOptions::new(vec![
            acp::SessionConfigOption::boolean("web_search", "Web Search", false)
                .category(acp::SessionConfigOptionCategory::Model),
        ]));
        let fs: Arc<dyn Fs> = FakeFs::new(cx.executor());
        let cx = cx.add_empty_window();
        let view = cx.update({
            move |window, cx| {
                let config_options: Rc<dyn AgentSessionConfigOptions> = config_options;
                let agent_server: Rc<dyn AgentServer> = agent_server;
                cx.new(|cx| ConfigOptionsView::new(config_options, agent_server, fs, window, cx))
            }
        });

        let handled = cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.toggle_category_picker(acp::SessionConfigOptionCategory::Model, window, cx)
            })
        });

        assert!(!handled);
    }

    #[gpui::test]
    fn unknown_config_options_survive_live_selector_updates(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let category = acp::SessionConfigOptionCategory::Mode;
        let unknown = acp::SessionConfigOption::new(
            "mode",
            "Mode",
            acp::SessionConfigKind::Other(acp::OtherSessionConfigKind::new(
                "_future",
                std::collections::BTreeMap::from([(
                    "currentValue".to_string(),
                    serde_json::json!({"opaque": [1, true]}),
                )]),
            )),
        )
        .category(category.clone());
        let config_options = Rc::new(TestSessionConfigOptions::new(vec![unknown.clone()]));
        let agent_server = Rc::new(TestAgentServer::default());
        let fs: Arc<dyn Fs> = FakeFs::new(cx.executor());
        let cx = cx.add_empty_window();
        let view = cx.update(|window, cx| {
            let config_options: Rc<dyn AgentSessionConfigOptions> = config_options.clone();
            let agent_server: Rc<dyn AgentServer> = agent_server.clone();
            cx.new(|cx| ConfigOptionsView::new(config_options, agent_server, fs, window, cx))
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(view.selectors.is_empty());
                assert_eq!(
                    view.config_option_ids,
                    vec![acp::SessionConfigId::new("mode")]
                );
                assert!(!view.toggle_category_picker(category.clone(), window, cx));
                assert!(!view.cycle_category_option(category.clone(), false, cx));
            });
        });
        assert_eq!(config_options.config_options(), vec![unknown.clone()]);

        config_options.replace_options(vec![
            acp::SessionConfigOption::select(
                "mode",
                "Mode",
                "auto",
                vec![
                    acp::SessionConfigSelectOption::new("auto", "Auto"),
                    acp::SessionConfigSelectOption::new("manual", "Manual"),
                ],
            )
            .category(category.clone()),
        ]);
        cx.run_until_parked();
        let selector = cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.selectors.len(), 1);
                assert!(view.toggle_category_picker(category.clone(), window, cx));
                assert!(view.cycle_category_option(category.clone(), false, cx));
                view.selectors.first().expect("select control").clone()
            })
        });
        let picker = cx.update(|_, cx| selector.read(cx).picker.clone().expect("select picker"));

        config_options.replace_options(vec![unknown.clone()]);
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(view.selectors.is_empty());
                assert!(!view.toggle_category_picker(category.clone(), window, cx));
                assert!(!view.cycle_category_option(category.clone(), false, cx));
            });
            assert!(!selector.update(cx, |selector, cx| selector.toggle_picker(window, cx)));
            picker.update(cx, |picker, cx| {
                assert!(!picker.delegate.can_select(0, window, cx));
                picker.delegate.confirm(false, window, cx);
            });
        });
        assert_eq!(config_options.config_options(), vec![unknown]);
        assert_eq!(
            config_options.set_values.borrow().as_slice(),
            &[(
                "mode".to_string(),
                acp::SessionConfigOptionValue::id("manual")
            )]
        );
        assert_eq!(
            agent_server.saved_defaults.lock().as_slice(),
            &[(
                "mode".to_string(),
                Some(AgentConfigOptionValue::ValueId("manual".to_string()))
            )]
        );
    }

    #[derive(Default)]
    struct TestAgentServer {
        saved_defaults: Arc<Mutex<Vec<(String, Option<AgentConfigOptionValue>)>>>,
    }

    impl AgentServer for TestAgentServer {
        fn logo(&self) -> IconName {
            IconName::ZedAssistant
        }

        fn agent_id(&self) -> AgentId {
            AgentId::new("test-agent")
        }

        fn connect(
            &self,
            _delegate: agent_servers::AgentServerDelegate,
            _project: Entity<Project>,
            _cx: &mut App,
        ) -> Task<anyhow::Result<Rc<dyn AgentConnection>>> {
            Task::ready(Err(anyhow::anyhow!("test agent server cannot connect")))
        }

        fn into_any(self: Rc<Self>) -> Rc<dyn Any> {
            self
        }

        fn set_default_config_option(
            &self,
            config_id: &str,
            value: Option<AgentConfigOptionValue>,
            _fs: Arc<dyn Fs>,
            _cx: &mut App,
        ) {
            self.saved_defaults
                .lock()
                .push((config_id.to_string(), value));
        }
    }

    struct TestSessionConfigOptions {
        options: RefCell<Vec<acp::SessionConfigOption>>,
        set_values: RefCell<Vec<(String, acp::SessionConfigOptionValue)>>,
        updates: RefCell<watch::Sender<()>>,
        next_response: RefCell<
            Option<
                futures::channel::oneshot::Receiver<anyhow::Result<Vec<acp::SessionConfigOption>>>,
            >,
        >,
    }

    impl TestSessionConfigOptions {
        fn new(options: Vec<acp::SessionConfigOption>) -> Self {
            Self {
                options: RefCell::new(options),
                set_values: RefCell::new(Vec::new()),
                updates: RefCell::new(watch::channel(()).0),
                next_response: RefCell::new(None),
            }
        }

        fn defer_next_response(
            &self,
        ) -> futures::channel::oneshot::Sender<anyhow::Result<Vec<acp::SessionConfigOption>>>
        {
            let (sender, receiver) = futures::channel::oneshot::channel();
            assert!(self.next_response.borrow_mut().replace(receiver).is_none());
            sender
        }

        fn replace_options(&self, options: Vec<acp::SessionConfigOption>) {
            *self.options.borrow_mut() = options;
            self.updates.borrow_mut().send(()).expect("config watcher");
        }
    }

    impl AgentSessionConfigOptions for TestSessionConfigOptions {
        fn config_options(&self) -> Vec<acp::SessionConfigOption> {
            self.options.borrow().clone()
        }

        fn set_config_option(
            &self,
            config_id: acp::SessionConfigId,
            value: acp::SessionConfigOptionValue,
            cx: &mut App,
        ) -> Task<anyhow::Result<Vec<acp::SessionConfigOption>>> {
            self.set_values
                .borrow_mut()
                .push((config_id.0.to_string(), value.clone()));

            if let Some(response) = self.next_response.borrow_mut().take() {
                return cx.spawn(async move |_| response.await?);
            }
            let options = {
                let mut options = self.options.borrow_mut();
                if let Some(option) = options
                    .iter_mut()
                    .find(|option| option.config_id == config_id)
                {
                    match (&mut option.kind, value) {
                        (
                            acp::SessionConfigKind::Select(select),
                            acp::SessionConfigOptionValue::Id { value },
                        ) => {
                            select.current_value = value;
                        }
                        (
                            acp::SessionConfigKind::Boolean(boolean),
                            acp::SessionConfigOptionValue::Boolean { value },
                        ) => {
                            boolean.current_value = value;
                        }
                        _ => {}
                    }
                }
                options.clone()
            };

            Task::ready(Ok(options))
        }

        fn watch(&self, _cx: &mut App) -> Option<watch::Receiver<()>> {
            Some(self.updates.borrow().receiver())
        }
    }
}
