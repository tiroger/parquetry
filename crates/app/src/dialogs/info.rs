//! About, keyboard shortcuts and help.

use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;

use crate::actions::SHORTCUTS;

pub fn about(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        dialog.width(px(420.)).child(
            v_flex()
                .items_center()
                .gap_2()
                .py_4()
                .child(div().text_2xl().font_weight(FontWeight::BOLD).child("Parquetry"))
                .child(div().text_sm().text_color(muted).child(format!("Version {}", env!("CARGO_PKG_VERSION"))))
                .child(div().text_sm().text_center().child("A fast viewer for Parquet, CSV, JSON, Arrow, Delta Lake and Iceberg data, on disk or in S3."))
                .child(div().text_xs().text_color(muted).child("Built with GPUI and DuckDB.")),
        )
    });
}

pub fn shortcuts(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let theme = cx.theme();
        dialog.title("Keyboard Shortcuts").width(px(520.)).child(
            v_flex().gap_1().children(SHORTCUTS.iter().map(|(keys, what)| {
                h_flex()
                    .justify_between()
                    .py_0p5()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_sm().child(what.to_string()))
                    .child(div().text_sm().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(keys.to_string()))
            })),
        )
    });
}

pub fn help(window: &mut Window, cx: &mut App) {
    const TIPS: &[(&str, &str)] = &[
        ("Opening data", "Drop files or folders on the window, use File ▸ Open, or run `parquetry path/or/s3/url` in Terminal. Folders of partitioned Parquet (year=2024/…) open as one dataset."),
        ("Column summaries", "Headers show each column's distribution. Hover a chart for details; a dot means the summary came from a sample (View ▸ Compute Exact Summaries scans everything)."),
        ("Sorting and filtering", "Click a header for sort, filter and column options. Right-click cells to filter by a value or copy in other formats. Search looks through every column."),
        ("Large files", "Only the rows on screen are read. Sorting a 20 GB file reads just the sort column, then fetches the rows you look at."),
        ("S3", "Uses your AWS profiles (Settings ▸ Amazon S3). Bucket regions are detected automatically. Browse with File ▸ Open S3 Location."),
        ("SQL", "The SQL tab runs DuckDB SQL. The current file is `t`; every open dataset is available by its name. Open results in a new tab to filter, summarize and export them."),
        ("Quick Look", "Press Space on a .parquet file in Finder for a preview of its schema and first rows."),
    ];
    window.open_dialog(cx, |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        dialog.title("Parquetry Help").width(px(600.)).child(v_flex().gap_3().children(TIPS.iter().map(|(title, body)| {
            v_flex()
                .gap_0p5()
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title.to_string()))
                .child(div().text_sm().text_color(muted).child(body.to_string()))
        })))
    });
}
