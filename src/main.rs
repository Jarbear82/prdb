// Prevent console window in addition to Slint window in Windows release builds when, e.g., starting the app via file manager. Ignored on other platforms.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::error::Error;
use surrealdb::engine::local::SurrealKv;
use surrealdb::types::SurrealValue;
use surrealdb::Surreal;

slint::include_modules!();

#[derive(Debug, SurrealValue, Clone)]
struct Person {
    title: String,
    name: String,
    marketing: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize file-based SurrealDB using SurrealKV engine stored in the "app_db" folder
    let db = Surreal::new::<SurrealKv>("app_db").await?;
    db.use_ns("main").use_db("main").await?;

    let ui = AppWindow::new()?;

    let db_clone_add = db.clone();
    let ui_handle_add = ui.as_weak();

    // Callback for adding a sample record
    ui.on_add_sample_record(move || {
        let db = db_clone_add.clone();
        let ui_handle = ui_handle_add.clone();
        tokio::spawn(async move {
            let res: surrealdb::Result<Option<Person>> = db
                .create("person")
                .content(Person {
                    title: "Software Engineer".to_string(),
                    name: "Alice Smith".to_string(),
                    marketing: true,
                })
                .await;

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_handle.upgrade() {
                    match res {
                        Ok(Some(p)) => {
                            ui.set_query_result(format!("Successfully created record:\n{:#?}", p).into());
                        }
                        Ok(None) => {
                            ui.set_query_result("Record created successfully (no return content).".into());
                        }
                        Err(e) => {
                            ui.set_query_result(format!("Error creating record: {}", e).into());
                        }
                    }
                }
            });
        });
    });

    let db_clone_query = db.clone();
    let ui_handle_query = ui.as_weak();

    // Callback for executing custom SurrealQL queries entered in the UI
    ui.on_execute_query(move || {
        let db = db_clone_query.clone();
        let ui_handle = ui_handle_query.clone();
        let ui_upgrade = ui_handle.upgrade();
        if ui_upgrade.is_none() {
            return;
        }
        let query_str = ui_upgrade.unwrap().get_query_text().to_string();

        tokio::spawn(async move {
            let mut result = db.query(&query_str).await;
            let output = match &mut result {
                Ok(res) => {
                    match res.take::<serde_json::Value>(0) {
                        Ok(val) => format!("{:#?}", val),
                        Err(e) => format!("Query executed, but failed to format response: {}", e),
                    }
                }
                Err(e) => format!("Query error: {}", e),
            };

            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_query_result(output.into());
                }
            });
        });
    });

    ui.run()?;

    Ok(())
}
