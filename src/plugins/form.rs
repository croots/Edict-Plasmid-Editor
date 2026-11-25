//! A settings form made from a plugin's manifest: chips for choices, a check box for
//! booleans, text fields for numbers and strings.

use std::collections::BTreeMap;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, EventEmitter, FontWeight, Subscription, Window};

use super::manifest::{Setting, SettingType};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;

enum Field {
    /// Choice and bool settings: the value as text ("true").
    Pick(String),
    Text(Entity<TextInput>),
}

pub struct SettingsForm {
    settings: Vec<Setting>,
    fields: Vec<Field>,
    /// Per-field problems from the last check, by index.
    problems: BTreeMap<usize, String>,
    _subs: Vec<Subscription>,
}

pub enum FormEvent {
    /// Enter in a text field.
    Submit,
}

impl EventEmitter<FormEvent> for SettingsForm {}

impl SettingsForm {
    /// `saved` holds earlier values by key; settings missing from it take their default.
    pub fn new(settings: Vec<Setting>, saved: &BTreeMap<String, String>, cx: &mut Context<Self>) -> Self {
        let mut subs = Vec::new();
        let fields = settings
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let text = saved.get(&s.key).filter(|v| s.value(v).is_ok()).cloned().unwrap_or_else(|| s.default_text());
                match s.ty {
                    SettingType::Choice | SettingType::Bool => Field::Pick(text),
                    _ => {
                        let input = cx.new(|cx| TextInput::new(&text, s.default_text(), cx));
                        subs.push(cx.subscribe(&input, move |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                            InputEvent::Changed => {
                                if this.problems.remove(&i).is_some() {
                                    cx.notify();
                                }
                            }
                            InputEvent::Submit => cx.emit(FormEvent::Submit),
                            _ => {}
                        }));
                        Field::Text(input)
                    }
                }
            })
            .collect();
        SettingsForm { settings, fields, problems: BTreeMap::new(), _subs: subs }
    }

    fn text(&self, i: usize, cx: &gpui::App) -> String {
        match &self.fields[i] {
            Field::Pick(v) => v.clone(),
            Field::Text(e) => e.read(cx).text().trim().to_string(),
        }
    }

    /// The typed values for the request, and their text to save; or marks the
    /// fields that don't parse and says which.
    pub fn values(&mut self, cx: &mut Context<Self>) -> Result<(serde_json::Map<String, serde_json::Value>, BTreeMap<String, String>), String> {
        let mut typed = serde_json::Map::new();
        let mut texts = BTreeMap::new();
        self.problems.clear();
        for (i, s) in self.settings.iter().enumerate() {
            let mut text = self.text(i, cx);
            if text.is_empty() {
                text = s.default_text();
            }
            match s.value(&text) {
                Ok(v) => {
                    typed.insert(s.key.clone(), v);
                    texts.insert(s.key.clone(), text);
                }
                Err(e) => {
                    self.problems.insert(i, e);
                }
            }
        }
        cx.notify();
        match self.problems.keys().next() {
            None => Ok((typed, texts)),
            Some(&i) => Err(format!("{}: {}", self.settings[i].label, self.problems[&i])),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.settings.is_empty()
    }

    fn set(&mut self, i: usize, value: String, cx: &mut Context<Self>) {
        if let Field::Pick(v) = &mut self.fields[i] {
            *v = value;
            self.problems.remove(&i);
            cx.notify();
        }
    }

    fn control(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let s = &self.settings[i];
        match (&self.fields[i], s.ty) {
            (Field::Pick(current), SettingType::Bool) => {
                let on = current == "true";
                div()
                    .id(("check", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .cursor_pointer()
                    .child(
                        div()
                            .size(px(14.0))
                            .rounded_sm()
                            .border_1()
                            .border_color(if on { theme::accent() } else { theme::border() })
                            .when(on, |d| d.bg(theme::accent()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(theme::editor_bg())
                            .child(if on { "✓" } else { "" }),
                    )
                    .child(if on { "On" } else { "Off" })
                    .on_click(cx.listener(move |this, _, _, cx| this.set(i, (!on).to_string(), cx)))
                    .into_any_element()
            }
            (Field::Pick(current), _) => {
                let help = s.choices.iter().find(|c| c.value() == current).and_then(|c| c.help()).map(str::to_string);
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().flex().flex_row().flex_wrap().gap_1().children(s.choices.iter().enumerate().map(|(k, c)| {
                        let on = c.value() == current;
                        let value = c.value().to_string();
                        div()
                            .id(("chip", i * 1000 + k))
                            .px_2()
                            .py(px(2.0))
                            .rounded_md()
                            .border_1()
                            .text_xs()
                            .cursor_pointer()
                            .when(on, |d| d.border_color(theme::accent()).bg(theme::accent()).text_color(theme::editor_bg()))
                            .when(!on, |d| d.border_color(theme::border()).hover(|s| s.bg(theme::hover())))
                            .child(c.label().to_string())
                            .on_click(cx.listener(move |this, _, _, cx| this.set(i, value.clone(), cx)))
                    })))
                    .children(help.map(|h| div().text_xs().text_color(theme::muted_text()).child(h)))
                    .into_any_element()
            }
            (Field::Text(input), ty) => {
                let narrow = matches!(ty, SettingType::Int | SettingType::Float);
                let bad = self.problems.contains_key(&i);
                div()
                    .when(narrow, |d| d.w(px(140.0)))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(if bad { theme::error() } else { theme::border() })
                    .bg(theme::app_bg())
                    .text_sm()
                    .child(input.clone())
                    .into_any_element()
            }
        }
    }
}

impl Render for SettingsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = |text: String| div().w(px(170.0)).flex_none().pt(px(2.0)).text_sm().text_color(theme::muted_text()).child(text);
        let mut d = div().flex().flex_col().gap_2();
        let mut group: Option<&str> = None;
        let settings = self.settings.clone();
        for (i, s) in settings.iter().enumerate() {
            if s.group.as_deref() != group {
                group = s.group.as_deref();
                if let Some(g) = group {
                    d = d.child(div().pt_2().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme::muted_text()).child(g.to_uppercase()));
                }
            }
            let mut right = div().flex_1().min_w_0().flex().flex_col().gap_1().child(self.control(i, cx));
            if let Some(p) = self.problems.get(&i) {
                right = right.child(div().text_xs().text_color(theme::error()).child(p.clone()));
            } else if let Some(h) = &s.help {
                right = right.child(div().text_xs().text_color(theme::muted_text()).child(h.clone()));
            }
            d = d.child(div().flex().flex_row().gap_3().child(label(s.label.clone())).child(right));
        }
        d
    }
}
