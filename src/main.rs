#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::error::Error;
use std::sync::Arc;

use serde_json::Value as JsonValue;
use slint::{Model, ModelRc, SharedString, VecModel};
use surrealdb::IndexedResults;
use surrealdb::Surreal;
use surrealdb::engine::local::SurrealKv;
use surrealdb::types::Value;

slint::include_modules!();

fn set_status(ui: &AppWindow, msg: impl Into<SharedString>, is_error: bool) {
    ui.set_status_message(msg.into());
    ui.set_status_is_error(is_error);
}

fn tables_to_model(names: Vec<String>) -> ModelRc<TableInfo> {
    let model = VecModel::from(
        names
            .into_iter()
            .map(|name| TableInfo { name: name.into() })
            .collect::<Vec<_>>(),
    );
    ModelRc::new(model)
}

fn fields_to_model(fields: Vec<(String, String, bool)>) -> ModelRc<FieldInfo> {
    let model = VecModel::from(
        fields
            .into_iter()
            .map(|(name, type_name, is_optional)| FieldInfo {
                name: name.into(),
                type_name: type_name.into(),
                is_optional,
            })
            .collect::<Vec<_>>(),
    );
    ModelRc::new(model)
}

fn records_to_model(rows: Vec<(String, String)>) -> ModelRc<RecordRow> {
    let model = VecModel::from(
        rows.into_iter()
            .map(|(id, summary)| RecordRow {
                id: id.into(),
                summary: summary.into(),
            })
            .collect::<Vec<_>>(),
    );
    ModelRc::new(model)
}

fn views_to_model(views: Vec<(String, String)>) -> ModelRc<ViewInfo> {
    let model = VecModel::from(
        views
            .into_iter()
            .map(|(name, definition)| ViewInfo {
                name: name.into(),
                definition: definition.into(),
            })
            .collect::<Vec<_>>(),
    );
    ModelRc::new(model)
}

fn main() -> Result<(), Box<dyn Error>> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(8 * 1024 * 1024) // 8 MB
        .enable_all()
        .build()?;

    rt.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn Error>> {
    let db = Arc::new(Surreal::new::<SurrealKv>("app_db").await?);
    db.use_ns("main").use_db("main").await?;

    let ui = AppWindow::new()?;
    let ui_weak = ui.as_weak();

    // ---------- Refresh Tables ----------
    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_refresh_tables(move || {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            tokio::spawn(async move {
                let result = db.query("INFO FOR DB;").await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(mut res) => {
                                let tables = extract_table_names(&mut res);
                                ui.set_tables(tables_to_model(tables));
                                set_status(&ui, "Tables refreshed", false);
                            }
                            Err(e) => set_status(&ui, format!("Error: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    // ---------- Create Table (SCHEMAFULL) ----------
    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_create_table(move || {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let name = ui_weak
                .upgrade()
                .map(|u| u.get_new_table_name().to_string())
                .unwrap_or_default();
            if name.trim().is_empty() {
                return;
            }
            let name = name.trim().to_string();
            tokio::spawn(async move {
                let q = format!("DEFINE TABLE {} SCHEMAFULL;", name);
                let result = db.query(&q).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(_) => {
                                ui.set_new_table_name("".into());
                                set_status(
                                    &ui,
                                    format!("Created SCHEMAFULL table '{}'", name),
                                    false,
                                );
                                ui.invoke_refresh_tables();
                            }
                            Err(e) => set_status(&ui, format!("Create table failed: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    // ---------- Select Table ----------
    {
        let ui_weak = ui_weak.clone();
        ui.on_select_table(move |idx| {
            if let Some(ui) = ui_weak.upgrade() {
                let tables = ui.get_tables();
                if idx >= 0 && (idx as usize) < tables.row_count() {
                    if let Some(t) = tables.row_data(idx as usize) {
                        ui.set_selected_table_index(idx);
                        ui.set_selected_table_name(t.name.clone());
                        ui.invoke_refresh_fields();
                        ui.invoke_refresh_records();
                        set_status(&ui, format!("Selected table '{}'", t.name), false);
                    }
                }
            }
        });
    }

    // ---------- Refresh Fields ----------
    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_refresh_fields(move || {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let table = ui_weak
                .upgrade()
                .map(|u| u.get_selected_table_name().to_string())
                .unwrap_or_default();
            if table.is_empty() {
                return;
            }
            tokio::spawn(async move {
                let q = format!("INFO FOR TABLE {};", table);
                let result = db.query(&q).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(mut res) => {
                                let fields = extract_fields(&mut res);
                                ui.set_fields(fields_to_model(fields));
                            }
                            Err(e) => set_status(&ui, format!("Refresh fields error: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    // ---------- Add Field ----------
    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_add_field(move || {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let (table, fname, ftype, optional, target) = if let Some(ui) = ui_weak.upgrade() {
                (
                    ui.get_selected_table_name().to_string(),
                    ui.get_new_field_name().to_string(),
                    ui.get_new_field_type().to_string(),
                    ui.get_new_field_optional(),
                    ui.get_new_field_record_target().to_string(),
                )
            } else {
                return;
            };

            if table.is_empty() || fname.trim().is_empty() {
                return;
            }

            let fname = fname.trim().to_string();
            tokio::spawn(async move {
                let type_clause = if ftype == "record" {
                    if target.trim().is_empty() {
                        "record".to_string()
                    } else {
                        format!("record<{}>", target.trim())
                    }
                } else {
                    ftype
                };

                let (opt_open, opt_close) = if optional { ("option<", ">") } else { ("", "") };

                let q = format!(
                    "DEFINE FIELD {} ON {} TYPE {}{}{};",
                    fname, table, opt_open, type_clause, opt_close
                );

                let result = db.query(&q).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(_) => {
                                ui.set_new_field_name("".into());
                                set_status(&ui, format!("Added field '{}'", fname), false);
                                ui.invoke_refresh_fields();
                            }
                            Err(e) => set_status(&ui, format!("Add field failed: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    // ---------- Refresh Records ----------
    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_refresh_records(move || {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let table = ui_weak
                .upgrade()
                .map(|u| u.get_selected_table_name().to_string())
                .unwrap_or_default();
            if table.is_empty() {
                return;
            }
            tokio::spawn(async move {
                let q = format!("SELECT * FROM {};", table);
                let result = db.query(&q).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(mut res) => {
                                let rows = extract_records(&mut res);
                                ui.set_records(records_to_model(rows));
                            }
                            Err(e) => set_status(&ui, format!("Refresh records error: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    // ---------- Create Record ----------
    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_create_record(move || {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let (table, json_str) = if let Some(ui) = ui_weak.upgrade() {
                (
                    ui.get_selected_table_name().to_string(),
                    ui.get_record_json().to_string(),
                )
            } else {
                return;
            };

            tokio::spawn(async move {
                let content: JsonValue = match serde_json::from_str(&json_str) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                set_status(&ui, format!("Invalid JSON: {e}"), true);
                            }
                        });
                        return;
                    }
                };

                // Explicit type annotation required in SurrealDB 4.0
                let result: Result<Option<Value>, _> =
                    db.create(table.as_str()).content(content).await;

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(_) => {
                                set_status(&ui, "Record created", false);
                                ui.invoke_refresh_records();
                            }
                            Err(e) => set_status(&ui, format!("Create failed: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    // ---------- Update Record ----------
    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_update_record(move || {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let (table, id, json_str) = if let Some(ui) = ui_weak.upgrade() {
                (
                    ui.get_selected_table_name().to_string(),
                    ui.get_editing_record_id().to_string(),
                    ui.get_record_json().to_string(),
                )
            } else {
                return;
            };

            if id.is_empty() {
                return;
            }

            tokio::spawn(async move {
                let content: JsonValue = match serde_json::from_str(&json_str) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak.upgrade() {
                                set_status(&ui, format!("Invalid JSON: {e}"), true);
                            }
                        });
                        return;
                    }
                };

                // Extract just the key part if the id is already "table:key"
                let key = if let Some((_, k)) = id.split_once(':') {
                    k.to_string()
                } else {
                    id.clone()
                };

                // Use (table, key) tuple → returns Option<Value>
                let result: Result<Option<Value>, _> = db
                    .update((table.as_str(), key.as_str()))
                    .content(content)
                    .await;

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(_) => {
                                ui.set_editing_record_id("".into());
                                set_status(&ui, "Record updated", false);
                                ui.invoke_refresh_records();
                            }
                            Err(e) => set_status(&ui, format!("Update failed: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    // ---------- Delete Record ----------
    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_delete_record(move |id| {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let id = id.to_string();

            tokio::spawn(async move {
                // Always produce Result<Option<Value>, _>
                let result: Result<Option<Value>, _> = match id.split_once(':') {
                    Some((table, key)) => {
                        // Normal case: "table:key"
                        db.delete((table, key)).await
                    }
                    None => {
                        // Bare key – we cannot form a proper record id.
                        // Return Ok(None) so the type stays Option<Value>.
                        // (Alternatively show an error in the UI.)
                        Ok(None)
                    }
                };

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(Some(_)) => {
                                set_status(&ui, "Record deleted", false);
                                ui.invoke_refresh_records();
                            }
                            Ok(None) => {
                                set_status(&ui, "Nothing deleted (invalid or missing id)", true);
                            }
                            Err(e) => set_status(&ui, format!("Delete failed: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    // ---------- Create Event ----------
    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_create_event(move || {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let (table, name, when, then) = if let Some(ui) = ui_weak.upgrade() {
                (
                    ui.get_selected_table_name().to_string(),
                    ui.get_new_event_name().to_string(),
                    ui.get_new_event_when().to_string(),
                    ui.get_new_event_then().to_string(),
                )
            } else {
                return;
            };

            if name.trim().is_empty() || then.trim().is_empty() {
                return;
            }

            let name = name.trim().to_string();
            tokio::spawn(async move {
                let q = format!(
                    "DEFINE EVENT {} ON TABLE {} WHEN ${} THEN {};",
                    name,
                    table,
                    when.to_lowercase(),
                    then.trim()
                );
                let result = db.query(&q).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(_) => {
                                ui.set_new_event_name("".into());
                                ui.set_new_event_then("".into());
                                set_status(&ui, format!("Event '{}' created", name), false);
                            }
                            Err(e) => set_status(&ui, format!("Create event failed: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    // ---------- Views ----------
    {
        let ui_weak = ui_weak.clone();
        ui.on_refresh_views(move || {
            let ui_weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_views(views_to_model(vec![]));
                    set_status(&ui, "Views list refreshed (basic)", false);
                }
            });
        });
    }

    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_create_view(move || {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let (name, def) = if let Some(ui) = ui_weak.upgrade() {
                (
                    ui.get_new_view_name().to_string(),
                    ui.get_new_view_definition().to_string(),
                )
            } else {
                return;
            };

            if name.trim().is_empty() || def.trim().is_empty() {
                return;
            }

            let name = name.trim().to_string();
            tokio::spawn(async move {
                let q = format!("DEFINE VIEW {} AS {};", name, def.trim());
                let result = db.query(&q).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(_) => {
                                ui.set_new_view_name("".into());
                                set_status(&ui, format!("View '{}' created", name), false);
                                ui.invoke_refresh_views();
                            }
                            Err(e) => set_status(&ui, format!("Create view failed: {e}"), true),
                        }
                    }
                });
            });
        });
    }

    {
        let db = db.clone();
        let ui_weak = ui_weak.clone();
        ui.on_open_view(move |name| {
            let db = db.clone();
            let ui_weak = ui_weak.clone();
            let name = name.to_string();
            tokio::spawn(async move {
                let q = format!("SELECT * FROM {};", name);
                let result = db.query(&q).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak.upgrade() {
                        match result {
                            Ok(mut res) => {
                                let text = format!("{:?}", res);
                                ui.set_view_result(text.into());
                                set_status(&ui, format!("Opened view '{}'", name), false);
                            }
                            Err(e) => {
                                ui.set_view_result(format!("Error: {e}").into());
                                set_status(&ui, format!("Open view failed: {e}"), true);
                            }
                        }
                    }
                });
            });
        });
    }

    // Clear status
    {
        let ui_weak = ui_weak.clone();
        ui.on_clear_status(move || {
            if let Some(ui) = ui_weak.upgrade() {
                set_status(&ui, "Ready", false);
            }
        });
    }

    // Initial load
    ui.invoke_refresh_tables();

    ui.run()?;
    Ok(())
}

// ===== Helper functions (SurrealDB 4.0 compatible) =====

fn extract_table_names(res: &mut IndexedResults) -> Vec<String> {
    // We keep the type as surrealdb::Response here because it is still the
    // concrete type returned by .query(), even if it is not re-exported at the root.
    match res.take::<Value>(0) {
        Ok(Value::Object(obj)) => {
            if let Some(Value::Object(tables)) = obj.get("tables") {
                tables.keys().cloned().collect()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn extract_fields(res: &mut IndexedResults) -> Vec<(String, String, bool)> {
    match res.take::<Value>(0) {
        Ok(Value::Object(obj)) => {
            if let Some(Value::Object(fields)) = obj.get("fields") {
                fields
                    .iter()
                    .map(|(name, def)| {
                        // Value does not implement Display – use Debug
                        let def_str = format!("{:?}", def);
                        let is_optional = def_str.contains("option<");
                        let type_name = if def_str.contains("TYPE record") {
                            "record".to_string()
                        } else if def_str.contains("option<") {
                            def_str
                                .split("option<")
                                .nth(1)
                                .and_then(|s| s.split('>').next())
                                .unwrap_or("unknown")
                                .to_string()
                        } else if let Some(t) = def_str.split("TYPE ").nth(1) {
                            t.split_whitespace().next().unwrap_or("unknown").to_string()
                        } else {
                            "unknown".to_string()
                        };
                        (name.clone(), type_name, is_optional)
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn extract_records(res: &mut IndexedResults) -> Vec<(String, String)> {
    match res.take::<Vec<Value>>(0) {
        Ok(rows) => rows
            .into_iter()
            .map(|v| {
                let id = match &v {
                    Value::Object(obj) => obj
                        .get("id")
                        .map(|i| format!("{:?}", i))
                        .unwrap_or_else(|| "unknown".into()),
                    _ => "unknown".into(),
                };
                let summary = format!("{:?}", v);
                (id, summary)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}
