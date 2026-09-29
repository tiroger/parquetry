//! Add Filter: a column condition or a free-form SQL WHERE clause.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use parquetry_engine::{ColumnInfo, Filter, FilterOp};

use crate::document::DatasetDocument;

type Names = Vec<SharedString>;

pub struct FilterForm {
    columns: Vec<ColumnInfo>,
    mode: usize,
    column: Entity<SelectState<SearchableVec<SharedString>>>,
    op: Entity<SelectState<Names>>,
    ops: Vec<FilterOp>,
    value: Entity<InputState>,
    value2: Entity<InputState>,
    where_sql: Entity<InputState>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl FilterForm {
    fn new(columns: Vec<ColumnInfo>, column: Option<usize>, where_sql: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let names: Names = columns.iter().map(|c| SharedString::from(c.name.clone())).collect();
        let selected = column.unwrap_or(0);
        let column_select =
            cx.new(|cx| SelectState::new(SearchableVec::new(names), Some(IndexPath::new(selected)), window, cx).searchable(true));
        let ops = columns.get(selected).map(|c| FilterOp::for_kind(c.kind)).unwrap_or_default();
        let op_select = cx.new(|cx| SelectState::new(op_labels(&ops), Some(IndexPath::new(0)), window, cx));
        let value = cx.new(|cx| InputState::new(window, cx).placeholder("Value"));
        let value2 = cx.new(|cx| InputState::new(window, cx).placeholder("Upper bound"));
        let where_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("e.g. amount > 100 AND region IN ('eu', 'us')")
                .default_value(where_sql)
        });
        let subscriptions = vec![
            cx.subscribe_in(&column_select, window, |this, _, event: &SelectEvent<SearchableVec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(Some(name)) = event else { return };
                if let Some(column) = this.columns.iter().find(|c| c.name == name.as_ref()) {
                    this.ops = FilterOp::for_kind(column.kind);
                    let labels = op_labels(&this.ops);
                    this.op.update(cx, |s, cx| {
                        s.set_items(labels, window, cx);
                        s.set_selected_index(Some(IndexPath::new(0)), window, cx);
                    });
                    cx.notify();
                }
            }),
            cx.subscribe_in(&op_select, window, |_, _, _: &SelectEvent<Names>, _, cx| cx.notify()),
            cx.subscribe_in(&value, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) && this.error.is_some() {
                    this.error = None;
                    cx.notify();
                }
            }),
        ];
        Self {
            columns,
            mode: 0,
            column: column_select,
            op: op_select,
            ops,
            value,
            value2,
            where_sql: where_input,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn selected_op(&self, cx: &App) -> Option<FilterOp> {
        let ix = self.op.read(cx).selected_index(cx)?.row;
        self.ops.get(ix).copied()
    }

    /// The filter described by the form, or a message saying what's missing.
    fn build(&self, cx: &App) -> Result<Choice, String> {
        if self.mode == 1 {
            return Ok(Choice::Where(self.where_sql.read(cx).value().trim().to_string()));
        }
        let column = self
            .column
            .read(cx)
            .selected_value()
            .cloned()
            .ok_or_else(|| "Choose a column".to_string())?;
        let op = self.selected_op(cx).ok_or_else(|| "Choose a condition".to_string())?;
        let value = self.value.read(cx).value().to_string();
        let value2 = self.value2.read(cx).value().to_string();
        let filter = if op == FilterOp::Between {
            Filter::between(column.to_string(), value, value2)
        } else {
            Filter::new(column.to_string(), op, value)
        };
        if let Some(info) = self.columns.iter().find(|c| c.name == column.as_ref()) {
            filter.to_sql(info, None).map_err(|e| e.to_string())?;
        }
        Ok(Choice::Filter(filter))
    }
}

enum Choice {
    Filter(Filter),
    Where(String),
}

fn op_labels(ops: &[FilterOp]) -> Names {
    ops.iter().map(|op| SharedString::from(op.label())).collect()
}

impl Render for FilterForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let arity = self.selected_op(cx).map(|op| op.arity()).unwrap_or(1);
        v_flex()
            .gap_3()
            .child(
                TabBar::new("filter-mode")
                    .segmented()
                    .small()
                    .selected_index(self.mode)
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.mode = *ix;
                        cx.notify();
                    }))
                    .child("Column condition")
                    .child("SQL expression"),
            )
            .when(self.mode == 0, |this| {
                this.child(Select::new(&self.column).placeholder("Column"))
                    .child(Select::new(&self.op).placeholder("Condition"))
                    .when(arity >= 1, |this| this.child(Input::new(&self.value)))
                    .when(arity == 2, |this| this.child(Input::new(&self.value2)))
                    .when(arity >= 1, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Text matching is case-insensitive. Separate several values with commas."),
                        )
                    })
            })
            .when(self.mode == 1, |this| {
                this.child(Input::new(&self.where_sql)).child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Any DuckDB boolean expression over this dataset's columns. Leave empty to remove."),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(theme.danger).child(error))
            })
    }
}

/// Open the dialog. `columns` and `where_sql` are passed in (rather than read from
/// `doc`) because this is called while `doc` itself is being updated.
pub fn open(
    doc: Entity<DatasetDocument>,
    columns: Vec<ColumnInfo>,
    where_sql: String,
    column: Option<usize>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| FilterForm::new(columns, column, where_sql, window, cx));
    let value = form.read(cx).value.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let form_ok = form.clone();
        let doc = doc.clone();
        dialog
            .title("Add Filter")
            .width(px(520.))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("cancel").label("Cancel").outline()))
                    .child(DialogAction::new().child(Button::new("apply").label("Apply Filter").primary())),
            )
            .on_ok(move |_, window, cx| match form_ok.read(cx).build(cx) {
                Ok(Choice::Filter(filter)) => {
                    doc.update(cx, |d, cx| d.add_filter(filter, window, cx));
                    true
                }
                Ok(Choice::Where(sql)) => {
                    doc.update(cx, |d, cx| d.set_where(sql, window, cx));
                    true
                }
                Err(message) => {
                    form_ok.update(cx, |f, cx| {
                        f.error = Some(message.into());
                        cx.notify();
                    });
                    false
                }
            })
    });
    crate::dialogs::focus_after_open(value, window);
}
