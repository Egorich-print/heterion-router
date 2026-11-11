//! Combos — the routing combos and their entries.
//!
//! Ported from `desktop/ui/src/views/Combos.svelte`. Every combo is listed
//! with its entries and the dispatch verdict of each entry, and the operator
//! can rename, reorder, restyle, hide or restock a combo. Every edit is one
//! `PATCH /api/combos/{id}`; the gateway reads combos from the database on
//! every request, so a change takes effect immediately.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, CursorStyle, Entity, FontWeight, Hsla, IntoElement, Render,
    SharedString, Window, div, px, rgba,
};

use crate::api::{self, Combo, ComboModelSpec, CombosResponse, UpdateComboBody};
use crate::theme;
use crate::widgets::{
    TextInput, Tone, cells_row, compact_badge, compact_button, error_banner, mono, muted,
    notice_banner, page_header, primary_button, table, table_row,
};

/// The subtitle of the page — the `.sub` paragraph of the Svelte view.
const SUBTITLE: &str = "Each entry is checked against the router's own dispatch rule, so what you see is what the gateway would actually try. An entry that is not servable either has no active credential or its provider has no executor yet. All edits — including the model list — apply immediately, no restart needed.";

/// The Combos section.
pub struct CombosView {
    /// The loaded payload.
    pub data: Option<CombosResponse>,
    /// The last failure, rendered as a banner.
    pub error: Option<String>,
    /// Whether a request is in flight.
    pub loading: bool,
    /// The notice of the last successful save.
    notice: Option<String>,
    /// Whether a patch is in flight — every control is dimmed meanwhile.
    saving: bool,
    /// Whether fully healthy combos are hidden from the list.
    hide_healthy: bool,
    /// The id of the combo being edited, if any.
    editing_id: Option<String>,
    /// The draft of the combo being edited.
    draft: Option<ComboDraft>,
    /// The provider field of the "add model" row.
    add_provider: String,
    /// The model field of the "add model" row.
    add_model: String,
    /// The weight field of the "add model" row.
    add_weight: i64,
    /// The name field of the edit form.
    name_input: Entity<TextInput>,
    /// The order field of the edit form.
    order_input: Entity<TextInput>,
    /// The provider field of the "add model" row.
    add_provider_input: Entity<TextInput>,
    /// The model field of the "add model" row.
    add_model_input: Entity<TextInput>,
    /// The weight field of the "add model" row.
    add_weight_input: Entity<TextInput>,
}

/// The fields of one combo while it is being edited.
#[derive(Debug, Clone, Default)]
struct ComboDraft {
    /// The new display name.
    name: String,
    /// The new ordering weight.
    order: i64,
    /// Whether the combo is hidden from clients.
    hidden: bool,
    /// `priority` or `auto`.
    strategy: String,
    /// The entries of the combo.
    models: Vec<DraftModel>,
}

/// One entry of a combo draft.
#[derive(Debug, Clone, Default)]
struct DraftModel {
    /// The entry id — round-tripped so the gateway keeps the row.
    id: String,
    /// The provider id.
    provider: String,
    /// The bare model id.
    model: String,
    /// The routing weight.
    weight: i64,
}

/// The gateway's answer to a combo patch.
#[derive(Debug, Clone, serde::Deserialize)]
struct ComboPatch {
    /// The name of the combo after the patch.
    #[serde(default)]
    name: String,
}

impl CombosView {
    /// Build the view and load its data.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            data: None,
            error: None,
            loading: true,
            notice: None,
            saving: false,
            hide_healthy: false,
            editing_id: None,
            draft: None,
            add_provider: String::new(),
            add_model: String::new(),
            add_weight: 50,
            name_input: draft_input(cx, "name", set_draft_name),
            order_input: draft_input(cx, "order", set_draft_order),
            add_provider_input: draft_input(cx, "provider", set_add_provider),
            add_model_input: draft_input(cx, "bare model id", set_add_model),
            add_weight_input: draft_input(cx, "weight", set_add_weight),
        };
        view.refresh(cx);
        view
    }

    /// Reload the data from the gateway.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            return;
        };
        self.loading = true;
        api::spawn_request(
            cx,
            move || client.get_json::<CombosResponse>("/api/combos"),
            |result, view: &mut CombosView, cx| {
                match result {
                    Ok(data) => {
                        view.data = Some(data);
                        view.error = None;
                    }
                    Err(error) => {
                        view.error = Some(error.to_string());
                        crate::app::report_error(cx, &error);
                    }
                }
                view.loading = false;
                cx.notify();
            },
        );
    }

    /// The combo with this id, if the list still holds it.
    fn combo(&self, id: &str) -> Option<Combo> {
        self.data
            .as_ref()?
            .combos
            .iter()
            .find(|combo| combo.id == id)
            .cloned()
    }

    /// Start editing a combo: the draft mirrors the current definition.
    fn start_edit(&mut self, combo: &Combo, cx: &mut Context<Self>) {
        let strategy = if combo.strategy.is_empty() {
            "priority".to_string()
        } else {
            combo.strategy.clone()
        };
        let models = combo
            .entries
            .iter()
            .map(|entry| DraftModel {
                id: entry.id.clone(),
                provider: entry.provider.clone(),
                model: entry.model.clone(),
                weight: entry.weight,
            })
            .collect();

        self.editing_id = Some(combo.id.clone());
        self.draft = Some(ComboDraft {
            name: combo.name.clone(),
            order: combo.sort_order,
            hidden: combo.is_hidden,
            strategy,
            models,
        });
        self.add_provider.clear();
        self.add_model.clear();
        self.add_weight = 50;
        self.notice = None;
        self.error = None;

        self.name_input
            .update(cx, |input, _| input.set_value(combo.name.clone()));
        self.order_input
            .update(cx, |input, _| input.set_value(combo.sort_order.to_string()));
        self.add_provider_input.update(cx, |input, _| input.clear());
        self.add_model_input.update(cx, |input, _| input.clear());
        self.add_weight_input
            .update(cx, |input, _| input.set_value("50"));
        cx.notify();
    }

    /// Leave the combo alone.
    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing_id = None;
        self.draft = None;
        cx.notify();
    }

    /// Change the strategy of the draft.
    fn set_strategy(&mut self, strategy: &str, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.as_mut() {
            draft.strategy = strategy.to_string();
        }
        cx.notify();
    }

    /// Flip the hidden flag of the draft.
    fn toggle_draft_hidden(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.as_mut() {
            draft.hidden = !draft.hidden;
        }
        cx.notify();
    }

    /// Move a weight by `delta`, clamped to the range the gateway accepts.
    fn bump_weight(&mut self, index: usize, delta: i64, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.as_mut()
            && let Some(model) = draft.models.get_mut(index)
        {
            model.weight = clamp_weight(model.weight + delta);
        }
        cx.notify();
    }

    /// Drop an entry from the draft.
    fn remove_model(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.as_mut()
            && index < draft.models.len()
        {
            draft.models.remove(index);
        }
        cx.notify();
    }

    /// Append the row of the "add model" form to the draft.
    fn add_model_row(&mut self, cx: &mut Context<Self>) {
        let provider = self.add_provider.trim().to_string();
        let mut model = self.add_model.trim().to_string();
        if provider.is_empty() || model.is_empty() {
            return;
        }
        // Paste convenience: "groq/openai/gpt-oss-20b" with provider "groq"
        // means the bare id "openai/gpt-oss-20b". Only the add form strips —
        // saved rows always round-trip untouched.
        if let Some(bare) = model.strip_prefix(&format!("{provider}/")) {
            model = bare.to_string();
        }
        if model.is_empty() {
            return;
        }

        let weight = clamp_weight(self.add_weight);
        if let Some(draft) = self.draft.as_mut() {
            draft.models.push(DraftModel {
                id: String::new(),
                provider,
                model,
                weight,
            });
        }
        self.add_provider.clear();
        self.add_model.clear();
        self.add_weight = 50;
        self.add_provider_input.update(cx, |input, _| input.clear());
        self.add_model_input.update(cx, |input, _| input.clear());
        self.add_weight_input
            .update(cx, |input, _| input.set_value("50"));
        cx.notify();
    }

    /// Send the whole draft to the gateway.
    fn save_full(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.clone() else {
            return;
        };
        let Some(id) = self.editing_id.clone() else {
            return;
        };
        if draft.name.trim().is_empty() || draft.models.is_empty() {
            return;
        }

        let body = UpdateComboBody {
            name: Some(draft.name.trim().to_string()),
            sort_order: Some(draft.order),
            is_hidden: Some(draft.hidden),
            strategy: Some(draft.strategy.clone()),
            models: Some(
                draft
                    .models
                    .iter()
                    .map(|model| ComboModelSpec {
                        id: if model.id.is_empty() {
                            None
                        } else {
                            Some(model.id.clone())
                        },
                        provider: model.provider.clone(),
                        model: model.model.clone(),
                        weight: Some(model.weight),
                    })
                    .collect(),
            ),
            ..Default::default()
        };
        self.patch(id, body, cx);
    }

    /// Send a patch to the gateway and reload the list.
    fn patch(&mut self, id: String, body: UpdateComboBody, cx: &mut Context<Self>) {
        let Some(client) = api::session_client(cx) else {
            return;
        };
        self.saving = true;
        self.error = None;
        self.notice = None;

        let path = format!("/api/combos/{id}");
        api::spawn_request(
            cx,
            move || client.patch_json::<ComboPatch, _>(&path, &body),
            |result, view: &mut CombosView, cx| {
                match result {
                    Ok(updated) => {
                        view.notice = Some(format!(
                            "Saved “{}”. Routing uses the new definition immediately.",
                            updated.name
                        ));
                        view.editing_id = None;
                        view.draft = None;
                    }
                    Err(error) => {
                        view.error = Some(error.to_string());
                        crate::app::report_error(cx, &error);
                    }
                }
                view.saving = false;
                view.refresh(cx);
                cx.notify();
            },
        );
    }

    /// The toolbar above the list.
    fn render_filter(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .mb(px(14.0))
            .child(checkbox(
                "combos-hide-healthy",
                "hide fully healthy combos",
                self.hide_healthy,
                move |_, cx| {
                    view.update(cx, |view, cx| {
                        view.hide_healthy = !view.hide_healthy;
                        cx.notify();
                    });
                },
            ))
            .into_any_element()
    }

    /// One combo: its header, the edit form and its entries.
    fn render_combo(&mut self, combo: &Combo, cx: &mut Context<Self>) -> AnyElement {
        let editing = self.editing_id.as_deref() == Some(combo.id.as_str());
        let saving = self.saving;
        let view = cx.entity();

        let servable = format!(
            "{}/{} servable",
            combo.servable_entries, combo.total_entries
        );
        // The CSS bundle paints the tag red when nothing is servable, even
        // when the combo is empty — the red rule comes last.
        let tone = if combo.servable_entries == 0 {
            Tone::Bad
        } else if combo.servable_entries == combo.total_entries {
            Tone::Ok
        } else {
            Tone::Neutral
        };

        let mut header = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .mt(px(22.0))
            .mb(px(8.0))
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(theme::muted())
                    .child(combo.name.clone()),
            )
            .child(header_chip(servable, tone))
            .child(header_chip(combo.strategy.clone(), Tone::Neutral));

        if combo.is_hidden {
            header = header.child(header_chip("hidden", Tone::Neutral));
        }
        header = header.child(header_chip(
            format!("order {}", combo.sort_order),
            Tone::Neutral,
        ));

        if editing {
            let view = view.clone();
            header = header.child(compact_button(
                format!("combo-cancel:{}", combo.id),
                "cancel",
                !saving,
                move |_, cx| {
                    view.update(cx, |view, cx| {
                        if view.saving {
                            return;
                        }
                        view.cancel_edit(cx);
                    });
                },
            ));
        } else {
            let edit_view = view.clone();
            let combo_id = combo.id.clone();
            header = header.child(compact_button(
                format!("combo-edit:{}", combo.id),
                "edit",
                !saving,
                move |_, cx| {
                    edit_view.update(cx, |view, cx| {
                        if view.saving {
                            return;
                        }
                        let Some(combo) = view.combo(&combo_id) else {
                            return;
                        };
                        view.start_edit(&combo, cx);
                    });
                },
            ));

            let hide_view = view.clone();
            let combo_id = combo.id.clone();
            let hidden = !combo.is_hidden;
            header = header.child(compact_button(
                format!("combo-hide:{}", combo.id),
                if combo.is_hidden { "unhide" } else { "hide" },
                !saving,
                move |_, cx| {
                    hide_view.update(cx, |view, cx| {
                        if view.saving {
                            return;
                        }
                        view.patch(
                            combo_id.clone(),
                            UpdateComboBody {
                                is_hidden: Some(hidden),
                                ..Default::default()
                            },
                            cx,
                        );
                    });
                },
            ));
        }

        let mut block = vec![header.into_any_element()];
        if editing {
            block.push(self.render_edit_form(cx).into_any_element());
        }
        block.push(self.render_entries(combo).into_any_element());

        div().flex().flex_col().children(block).into_any_element()
    }

    /// The edit form of the combo being edited.
    fn render_edit_form(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = self.draft.clone() else {
            return div().into_any_element();
        };
        let saving = self.saving;
        let view = cx.entity();

        let mut options: Vec<AnyElement> = Vec::new();
        for value in ["priority", "auto"] {
            let active = value == draft.strategy;
            let label = value.to_string();
            let picked = label.clone();
            let view = view.clone();
            options.push(
                div()
                    .id(theme::element_id(format!("combo-strategy:{value}")))
                    .flex()
                    .items_center()
                    .px(px(9.0))
                    .py(px(6.0))
                    .rounded(px(theme::RADIUS_SM))
                    .border_1()
                    .border_color(if active {
                        theme::accent()
                    } else {
                        theme::border()
                    })
                    .bg(if active {
                        theme::panel_2()
                    } else {
                        theme::panel()
                    })
                    .text_size(px(13.0))
                    .text_color(if active {
                        theme::text()
                    } else {
                        theme::muted()
                    })
                    .cursor(CursorStyle::PointingHand)
                    .on_click(move |_, _window, cx| {
                        view.update(cx, |view, cx| {
                            if view.saving {
                                return;
                            }
                            view.set_strategy(&picked, cx);
                        });
                    })
                    .child(label)
                    .into_any_element(),
            );
        }

        let form = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_end()
            .gap_3()
            .mb(px(14.0))
            .child(field("name", self.name_input.clone(), 220.0))
            .child(field("order", self.order_input.clone(), 90.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::muted())
                            .child("strategy"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .children(options),
                    ),
            )
            .child(div().flex().items_center().pb(px(8.0)).child(checkbox(
                "combos-draft-hidden",
                "hidden",
                draft.hidden,
                move |_, cx| {
                    view.update(cx, |view, cx| {
                        if view.saving {
                            return;
                        }
                        view.toggle_draft_hidden(cx);
                    });
                },
            )));

        let can_save = !saving && !draft.name.trim().is_empty() && !draft.models.is_empty();
        let view = cx.entity();
        let mut save_row = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_3()
            .mb(px(14.0))
            .child(primary_button(
                "combos-save",
                if saving { "saving…" } else { "Save combo" },
                can_save,
                move |_, cx| {
                    view.update(cx, |view, cx| {
                        if view.saving {
                            return;
                        }
                        view.save_full(cx);
                    });
                },
            ));
        if draft.models.is_empty() {
            save_row = save_row.child(muted("A combo needs at least one model."));
        }

        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            cells_row(vec![
                header_cell("provider / model"),
                header_cell("weight"),
                header_cell(""),
            ])
            .into_any_element(),
        );
        for (index, model) in draft.models.iter().enumerate() {
            rows.push(
                self.render_draft_row(index, model, saving, cx)
                    .into_any_element(),
            );
        }
        rows.push(self.render_add_row(saving, cx).into_any_element());

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(form)
            .child(table(rows))
            .child(save_row)
            .into_any_element()
    }

    /// One row of the draft model list: provider, weight and the remove button.
    fn render_draft_row(
        &self,
        index: usize,
        model: &DraftModel,
        saving: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();

        let less = view.clone();
        let more = view.clone();
        let remove = view.clone();

        let cells = vec![
            div()
                .flex_1()
                .flex()
                .items_center()
                .min_w_0()
                .child(mono(format!("{}/{}", model.provider, model.model)))
                .into_any_element(),
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(compact_button(
                    format!("draft-weight-down:{index}"),
                    "−",
                    !saving,
                    move |_, cx| {
                        less.update(cx, |view, cx| {
                            if view.saving {
                                return;
                            }
                            view.bump_weight(index, -5, cx);
                        });
                    },
                ))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme::text())
                        .child(model.weight.to_string()),
                )
                .child(compact_button(
                    format!("draft-weight-up:{index}"),
                    "+",
                    !saving,
                    move |_, cx| {
                        more.update(cx, |view, cx| {
                            if view.saving {
                                return;
                            }
                            view.bump_weight(index, 5, cx);
                        });
                    },
                ))
                .into_any_element(),
            div()
                .flex_1()
                .flex()
                .items_center()
                .min_w_0()
                .child(compact_button(
                    format!("draft-remove:{index}"),
                    "✕",
                    !saving,
                    move |_, cx| {
                        remove.update(cx, |view, cx| {
                            if view.saving {
                                return;
                            }
                            view.remove_model(index, cx);
                        });
                    },
                ))
                .into_any_element(),
        ];

        table_row(cells).into_any_element()
    }

    /// The row that adds one more model to the draft.
    fn render_add_row(&self, saving: bool, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let can_add = !self.add_provider.trim().is_empty() && !self.add_model.trim().is_empty();

        let cells = vec![
            div()
                .flex_1()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(self.add_provider_input.clone()),
                )
                .child(div().flex_1().min_w_0().child(self.add_model_input.clone()))
                .into_any_element(),
            div()
                .flex_1()
                .flex()
                .items_center()
                .min_w_0()
                .child(div().w(px(90.0)).child(self.add_weight_input.clone()))
                .into_any_element(),
            div()
                .flex_1()
                .flex()
                .items_center()
                .min_w_0()
                .child(compact_button(
                    "draft-add",
                    "add",
                    !saving && can_add,
                    move |_, cx| {
                        view.update(cx, |view, cx| {
                            if view.saving {
                                return;
                            }
                            view.add_model_row(cx);
                        });
                    },
                ))
                .into_any_element(),
        ];

        table_row(cells).into_any_element()
    }

    /// The entries of one combo: provider, model, backend and the verdict.
    fn render_entries(&self, combo: &Combo) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            cells_row(vec![
                header_cell("provider"),
                header_cell("model"),
                header_cell("backend"),
                header_cell("status"),
            ])
            .into_any_element(),
        );

        for entry in &combo.entries {
            let backend = entry.backend.clone().unwrap_or_else(|| "—".to_string());
            rows.push(
                cells_row(vec![
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .min_w_0()
                        .child(mono(entry.provider.clone()))
                        .into_any_element(),
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .min_w_0()
                        .child(mono(entry.model.clone()))
                        .into_any_element(),
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .min_w_0()
                        .child(mono_muted(backend))
                        .into_any_element(),
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .min_w_0()
                        .child(if entry.servable {
                            compact_badge("servable", Tone::Ok).into_any_element()
                        } else {
                            compact_badge("no backend", Tone::Bad).into_any_element()
                        })
                        .into_any_element(),
                ])
                .into_any_element(),
            );
        }

        table(rows).into_any_element()
    }
}

impl Render for CombosView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut blocks: Vec<AnyElement> = Vec::new();

        blocks.push(page_header("Combos", SUBTITLE).into_any_element());

        if let Some(error) = self.error.clone() {
            blocks.push(banner(error_banner(error)).into_any_element());
        }
        if let Some(notice) = self.notice.clone() {
            blocks.push(banner(notice_banner(notice)).into_any_element());
        }

        blocks.push(self.render_filter(cx).into_any_element());

        let shown: Vec<Combo> = self
            .data
            .as_ref()
            .map(|data| data.combos.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|combo| !self.hide_healthy || combo.servable_entries != combo.total_entries)
            .collect();

        if self.data.is_some() {
            if shown.is_empty() {
                blocks.push(muted("Nothing to show.").into_any_element());
            } else {
                for combo in &shown {
                    blocks.push(self.render_combo(combo, cx).into_any_element());
                }
            }
        } else if self.error.is_none() {
            blocks.push(muted("loading…").into_any_element());
        }

        div()
            .id(theme::element_id("combos-page"))
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .p(px(26.0))
                    .pb(px(40.0))
                    .gap(px(12.0))
                    .children(blocks),
            )
    }
}

/// Create a text field that keeps the view's state in sync.
fn draft_input(
    cx: &mut Context<CombosView>,
    placeholder: &str,
    apply: fn(&mut CombosView, &str),
) -> Entity<TextInput> {
    let weak = cx.entity().downgrade();
    let placeholder = placeholder.to_string();
    cx.new(|cx| {
        let mut input = TextInput::new(cx, "");
        input.set_placeholder(placeholder.clone());
        input.set_on_change(move |value, cx| {
            let _ = weak.update(cx, |view, cx| {
                apply(view, value);
                cx.notify();
            });
        });
        input
    })
}

/// The name of the combo being edited.
fn set_draft_name(view: &mut CombosView, value: &str) {
    if let Some(draft) = view.draft.as_mut() {
        draft.name = value.to_string();
    }
}

/// The order of the combo being edited.
fn set_draft_order(view: &mut CombosView, value: &str) {
    if let Some(draft) = view.draft.as_mut()
        && let Ok(order) = value.trim().parse::<i64>()
    {
        draft.order = order;
    }
}

/// The provider of the "add model" row.
fn set_add_provider(view: &mut CombosView, value: &str) {
    view.add_provider = value.to_string();
}

/// The model of the "add model" row.
fn set_add_model(view: &mut CombosView, value: &str) {
    view.add_model = value.to_string();
}

/// The weight of the "add model" row.
fn set_add_weight(view: &mut CombosView, value: &str) {
    if let Ok(weight) = value.trim().parse::<i64>() {
        view.add_weight = clamp_weight(weight);
    }
}

/// The weight range the gateway accepts.
fn clamp_weight(weight: i64) -> i64 {
    weight.clamp(0, 100)
}

/// A labelled text field.
fn field(label: &str, input: Entity<TextInput>, width: f32) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .w(px(width))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme::muted())
                .child(label.to_string()),
        )
        .child(input)
        .into_any_element()
}

/// A header cell of a table with more than three columns.
fn header_cell(text: &str) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .min_w_0()
        .text_size(px(11.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::muted())
        .child(text.to_string())
        .into_any_element()
}

/// Monospaced muted text — the `.mono.muted` cell of the CSS bundle.
fn mono_muted(text: impl Into<SharedString>) -> AnyElement {
    div()
        .font(gpui::font(theme::MONO_FAMILY))
        .text_size(px(12.0))
        .text_color(theme::muted())
        .child(text.into())
        .into_any_element()
}

/// A checkbox — gpui has no `<input type="checkbox">`.
fn checkbox(
    key: impl Into<SharedString>,
    label: impl Into<SharedString>,
    checked: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let mark = if checked {
        div()
            .text_size(px(10.0))
            .text_color(Hsla::from(rgba(0x06101fff)))
            .child("✓")
            .into_any_element()
    } else {
        div().into_any_element()
    };

    div()
        .id(theme::element_id(key))
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(13.0))
        .text_color(theme::text())
        .cursor(CursorStyle::PointingHand)
        .on_click(move |_, window, cx| on_toggle(window, cx))
        .child(
            div()
                .w(px(14.0))
                .h(px(14.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(if checked {
                    theme::accent()
                } else {
                    theme::border()
                })
                .bg(if checked {
                    theme::accent()
                } else {
                    theme::panel()
                })
                .flex()
                .items_center()
                .justify_center()
                .child(mark),
        )
        .child(label.into())
        .into_any_element()
}

/// A compact chip for the combo header (status, strategy, order, flags).
///
/// Same height, radius and type size as the header actions so the whole row
/// reads as one family. `flex_none` keeps it at the size of its content.
fn header_chip(label: impl Into<SharedString>, tone: Tone) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .h(px(24.0))
        .px(px(8.0))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(tone.color())
        .text_size(px(11.0))
        .text_color(tone.color())
        .child(label.into())
        .into_any_element()
}

/// A banner that spans the whole content width.
///
/// The shared banners carry the padding of the shell's content area; the view
/// brings its own, so the margins are cancelled here.
fn banner(element: impl IntoElement) -> AnyElement {
    div()
        .mx(px(-26.0))
        .mt(px(-22.0))
        .child(element)
        .into_any_element()
}
