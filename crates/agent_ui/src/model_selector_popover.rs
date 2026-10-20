use std::rc::Rc;

use acp_thread::{AgentModelIcon, AgentModelInfo, AgentModelSelector};
use gpui::{Entity, FocusHandle};
use picker::popover_menu::PickerPopoverMenu;
use ui::{PopoverMenuHandle, Tooltip, prelude::*};

use crate::ui::{ModelSelectorTooltip, model_label_color};
use crate::{ModelSelector, model_selector::acp_model_selector};

pub struct ModelSelectorPopover {
    selector: Entity<ModelSelector>,
    menu_handle: PopoverMenuHandle<ModelSelector>,
}

impl ModelSelectorPopover {
    pub(crate) fn new(
        selector: Rc<dyn AgentModelSelector>,
        menu_handle: PopoverMenuHandle<ModelSelector>,
        focus_handle: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            selector: cx
                .new(move |cx| acp_model_selector(selector, focus_handle.clone(), window, cx)),
            menu_handle,
        }
    }

    pub fn toggle(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_handle.toggle(window, cx);
    }

    pub fn active_model<'a>(&self, cx: &'a App) -> Option<&'a AgentModelInfo> {
        self.selector.read(cx).delegate.active_model()
    }

    pub fn cycle_favorite_models(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.selector.update(cx, |selector, cx| {
            selector.delegate.cycle_favorite_models(window, cx);
        });
    }
}

impl Render for ModelSelectorPopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selector = self.selector.read(cx);
        let model = selector.delegate.active_model();
        let model_name = model
            .as_ref()
            .map(|model| model.name.clone())
            .unwrap_or_else(|| SharedString::from("Select a Model"));

        let model_icon = model.as_ref().and_then(|model| model.icon.clone());

        let (color, icon) = if self.menu_handle.is_deployed() {
            (Color::Accent, IconName::ChevronUp)
        } else {
            (Color::Muted, IconName::ChevronDown)
        };
        let colored_label = if self.menu_handle.is_deployed() {
            None
        } else {
            model
                .as_ref()
                .and_then(|model| model_label_color(&[&model.name, model.id.as_str()], cx))
        };
        let (label_color, icon_color, chevron_color) = match colored_label {
            Some(colored_label) => (
                colored_label.label_color(cx),
                colored_label.icon_color(color, cx),
                colored_label.icon_color(Color::Muted, cx),
            ),
            None => (color, color, Color::Muted),
        };

        let show_cycle_row = selector.delegate.favorites_count() > 1;

        let tooltip = Tooltip::element({
            move |_, _cx| {
                ModelSelectorTooltip::new()
                    .show_cycle_row(show_cycle_row)
                    .into_any_element()
            }
        });

        PickerPopoverMenu::new(
            self.selector.clone(),
            Button::new("active-model", model_name)
                .label_size(LabelSize::Small)
                .color(label_color)
                .when_some(colored_label, |this, colored_label| {
                    this.style(colored_label.button_style())
                })
                .when_some(model_icon, |this, icon| {
                    this.start_icon(
                        match icon {
                            AgentModelIcon::Path(path) => Icon::from_external_svg(path),
                            AgentModelIcon::Named(icon_name) => Icon::new(icon_name),
                        }
                        .color(icon_color)
                        .size(IconSize::XSmall),
                    )
                })
                .end_icon(Icon::new(icon).color(chevron_color).size(IconSize::XSmall)),
            tooltip,
            gpui::Anchor::BottomRight,
            cx,
        )
        .with_handle(self.menu_handle.clone())
        .render(window, cx)
    }
}
