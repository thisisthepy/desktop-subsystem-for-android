//! The Widgets page's model: what `dsa widgets list` and `dsa widgets watch` print, as the
//! page reads it.

use dsa_settings_linux::model::{
    app_icon_name, PlacedWidget, WidgetChange, Widgets, WIDGETS_WATCH,
};
use serde_json::json;

fn placed(id: i64, x: i64) -> serde_json::Value {
    json!({
        "id": id,
        "provider": "org.example/.Clock",
        "package": "org.example",
        "label": "Clock",
        "app": "Example",
        "placement": {"x": x, "y": 20, "width_dp": 180, "height_dp": 110},
    })
}

#[test]
fn fr12_the_widget_page_lists_every_widget_with_its_app_name_preview_and_size() {
    let list = Widgets::read(&json!({
        "providers": [
            {
                "provider": "org.example/.Clock",
                "package": "org.example",
                "label": "Clock",
                "app": "Example",
                "preview_png": "iVBORw0KGgo=",
                "default_width_dp": 180,
                "default_height_dp": 110,
                "needs_configuration": true,
            },
            {"provider": "org.other/.Notes", "label": "Notes", "default_width_dp": 110, "default_height_dp": 40},
        ],
        "widgets": [placed(3, 10)],
    }));
    let clock = &list.offers[0];
    assert_eq!(
        (clock.label.as_str(), clock.app.as_str()),
        ("Clock", "Example")
    );
    assert_eq!((clock.width_dp, clock.height_dp), (180, 110));
    assert!(clock.needs_configuration);
    assert!(!clock.preview_png.is_empty());
    // A widget without a preview shows its app's icon, which the compositor keeps in the icon
    // theme by the app's package; the package comes from the provider when the list has none.
    let notes = &list.offers[1];
    assert!(notes.preview_png.is_empty());
    assert_eq!(notes.package, "org.other");
    assert_eq!(app_icon_name(&notes.package), "dsa-org.other");
    assert_eq!(list.placed[0].id, 3);
}

#[test]
fn fr12_placing_or_removing_a_widget_shows_on_the_page_without_a_restart() {
    assert_eq!(WIDGETS_WATCH, ["widgets", "watch", "--json"]);
    let mut list = Widgets::read(&json!({"providers": [], "widgets": [placed(3, 10)]}));
    // Placed elsewhere (its window's menu, dsa), moved, and removed: each line of the watch.
    let lines = [
        json!({"event": "placed", "widget": placed(5, 40)}).to_string(),
        "not json".to_string(),
        json!({"event": "placed", "widget": placed(3, 64)}).to_string(),
        json!({"event": "removed", "id": 5}).to_string(),
    ];
    for line in &lines {
        if let Some(change) = WidgetChange::read(line) {
            list.apply(change);
        }
    }
    assert_eq!(list.placed.len(), 1);
    assert_eq!((list.placed[0].id, list.placed[0].x), (3, 64));
    // The watch's first line lists them all.
    let all = WidgetChange::read(
        &json!({"event": "widgets", "widgets": [placed(1, 0), placed(2, 0)]}).to_string(),
    );
    let Some(WidgetChange::All(widgets)) = all else {
        panic!("not all the widgets: {all:?}");
    };
    assert_eq!(
        widgets
            .iter()
            .map(|w: &PlacedWidget| w.id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(WidgetChange::read(r#"{"event":"other"}"#), None);
}
