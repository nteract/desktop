use std::collections::HashMap;
use tauri::menu::{
    AboutMetadata, AboutMetadataBuilder, Menu, MenuItem, PredefinedMenuItem, Submenu,
};
use tauri::{AppHandle, Manager, Wry};

pub struct BundledSampleNotebook {
    pub id: &'static str,
    pub title: &'static str,
    pub file_name: &'static str,
    pub contents: &'static str,
}

// Menu item IDs for new notebook types
pub const MENU_NEW_NOTEBOOK: &str = "new_notebook";
pub const MENU_NEW_PYTHON_NOTEBOOK: &str = "new_python_notebook";
pub const MENU_NEW_DENO_NOTEBOOK: &str = "new_deno_notebook";
pub const MENU_OPEN: &str = "open";
pub const MENU_OPEN_SAMPLE_PREFIX: &str = "open_sample:";
pub const MENU_SAVE: &str = "save";
pub const MENU_CLONE_NOTEBOOK: &str = "clone_notebook";
pub const MENU_WINDOW_FOCUS_PREFIX: &str = "focus_window:";

// Menu item IDs for zoom
pub const MENU_ZOOM_IN: &str = "zoom_in";
pub const MENU_ZOOM_OUT: &str = "zoom_out";
pub const MENU_ZOOM_RESET: &str = "zoom_reset";

// Menu item IDs for kernel operations
pub const MENU_RUN_ALL_CELLS: &str = "run_all_cells";
pub const MENU_RESTART_AND_RUN_ALL: &str = "restart_and_run_all";

// Menu item IDs for cell operations
pub const MENU_INSERT_CODE_CELL: &str = "insert_code_cell";
pub const MENU_INSERT_MARKDOWN_CELL: &str = "insert_markdown_cell";
pub const MENU_INSERT_RAW_CELL: &str = "insert_raw_cell";
pub const MENU_CLEAR_OUTPUTS: &str = "clear_outputs";
pub const MENU_CLEAR_ALL_OUTPUTS: &str = "clear_all_outputs";

// Menu item IDs for CLI installation and settings
pub const MENU_INSTALL_CLI: &str = "install_cli";
pub const MENU_CHECK_FOR_UPDATES: &str = "check_for_updates";
pub const MENU_SETTINGS: &str = "settings";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_COMMIT_SHA: &str = env!("GIT_COMMIT");
pub const APP_RELEASE_DATE: &str = env!("GIT_COMMIT_DATE");

pub const BUNDLED_SAMPLE_NOTEBOOKS: &[BundledSampleNotebook] = &[
    BundledSampleNotebook {
        id: "markdown-and-math",
        title: "Meet Markdown and Math",
        file_name: "meet-markdown-and-math.ipynb",
        contents: include_str!("../resources/sample-notebooks/meet-markdown-and-math.ipynb"),
    },
    BundledSampleNotebook {
        id: "pandas-to-geojson",
        title: "Go from Pandas to GeoJSON",
        file_name: "pandas-to-geojson.ipynb",
        contents: include_str!("../resources/sample-notebooks/pandas-to-geojson.ipynb"),
    },
    BundledSampleNotebook {
        id: "download-stats",
        title: "Glean the Download Statistics for nteract Desktop",
        file_name: "download-stats.ipynb",
        contents: include_str!("../resources/sample-notebooks/download-stats.ipynb"),
    },
];

pub fn sample_menu_item_id(sample_id: &str) -> String {
    format!("{MENU_OPEN_SAMPLE_PREFIX}{sample_id}")
}

pub fn sample_for_menu_item_id(menu_id: &str) -> Option<&'static BundledSampleNotebook> {
    let sample_id = menu_id.strip_prefix(MENU_OPEN_SAMPLE_PREFIX)?;
    BUNDLED_SAMPLE_NOTEBOOKS
        .iter()
        .find(|sample| sample.id == sample_id)
}

pub fn app_name() -> &'static str {
    runt_workspace::desktop_display_name()
}

pub fn about_menu_label() -> String {
    format!("About {}", app_name())
}

pub fn install_cli_menu_label() -> String {
    format!(
        "Install '{}' Command in PATH...",
        runt_workspace::cli_command_name()
    )
}

pub fn window_menu_item_id(window_label: &str) -> String {
    format!("{MENU_WINDOW_FOCUS_PREFIX}{window_label}")
}

pub fn window_label_for_menu_item_id(menu_id: &str) -> Option<&str> {
    menu_id.strip_prefix(MENU_WINDOW_FOCUS_PREFIX)
}

fn build_about_metadata() -> AboutMetadata<'static> {
    AboutMetadataBuilder::new()
        .name(Some(app_name()))
        .version(Some(APP_VERSION))
        .comments(Some(format!(
            "Commit SHA: {APP_COMMIT_SHA}\nRelease Date: {APP_RELEASE_DATE}"
        )))
        .build()
}

/// Build the application menu bar
pub fn create_menu(
    app: &AppHandle,
    window_display_names: &HashMap<String, String>,
) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;
    let about_metadata = build_about_metadata();
    let about_label = about_menu_label();
    let install_cli_label = install_cli_menu_label();

    // App menu (macOS standard - shows app name)
    let app_menu = Submenu::new(app, app_name(), true)?;
    app_menu.append(&PredefinedMenuItem::about(
        app,
        Some(about_label.as_str()),
        Some(about_metadata),
    )?)?;
    app_menu.append(&PredefinedMenuItem::separator(app)?)?;
    app_menu.append(&MenuItem::with_id(
        app,
        MENU_INSTALL_CLI,
        install_cli_label.as_str(),
        true,
        None::<&str>,
    )?)?;
    app_menu.append(&MenuItem::with_id(
        app,
        MENU_CHECK_FOR_UPDATES,
        "Check for Updates...",
        true,
        None::<&str>,
    )?)?;
    app_menu.append(&PredefinedMenuItem::separator(app)?)?;
    app_menu.append(&MenuItem::with_id(
        app,
        MENU_SETTINGS,
        "Settings...",
        true,
        Some("CmdOrCtrl+,"),
    )?)?;
    app_menu.append(&PredefinedMenuItem::separator(app)?)?;
    app_menu.append(&PredefinedMenuItem::services(app, None)?)?;
    app_menu.append(&PredefinedMenuItem::separator(app)?)?;
    app_menu.append(&PredefinedMenuItem::hide(app, None)?)?;
    app_menu.append(&PredefinedMenuItem::hide_others(app, None)?)?;
    app_menu.append(&PredefinedMenuItem::show_all(app, None)?)?;
    app_menu.append(&PredefinedMenuItem::separator(app)?)?;
    app_menu.append(&PredefinedMenuItem::quit(app, None)?)?;
    menu.append(&app_menu)?;

    // File menu
    let file_menu = Submenu::new(app, "File", true)?;

    // New Notebook: Cmd+N uses the user's default runtime setting
    file_menu.append(&MenuItem::with_id(
        app,
        MENU_NEW_NOTEBOOK,
        "New Notebook",
        true,
        Some("CmdOrCtrl+N"),
    )?)?;

    // Explicit runtime overrides in a submenu
    let new_notebook_submenu = Submenu::new(app, "New Notebook As...", true)?;
    new_notebook_submenu.append(&MenuItem::with_id(
        app,
        MENU_NEW_PYTHON_NOTEBOOK,
        "Python",
        true,
        None::<&str>,
    )?)?;
    new_notebook_submenu.append(&MenuItem::with_id(
        app,
        MENU_NEW_DENO_NOTEBOOK,
        "Deno (TypeScript)",
        true,
        None::<&str>,
    )?)?;
    file_menu.append(&new_notebook_submenu)?;

    file_menu.append(&MenuItem::with_id(
        app,
        MENU_OPEN,
        "Open...",
        true,
        Some("CmdOrCtrl+O"),
    )?)?;

    let sample_submenu = Submenu::new(app, "Sample Notebooks", true)?;
    for sample in BUNDLED_SAMPLE_NOTEBOOKS {
        sample_submenu.append(&MenuItem::with_id(
            app,
            sample_menu_item_id(sample.id),
            sample.title,
            true,
            None::<&str>,
        )?)?;
    }
    file_menu.append(&sample_submenu)?;
    file_menu.append(&PredefinedMenuItem::separator(app)?)?;
    file_menu.append(&MenuItem::with_id(
        app,
        MENU_SAVE,
        "Save",
        true,
        Some("CmdOrCtrl+S"),
    )?)?;
    file_menu.append(&MenuItem::with_id(
        app,
        MENU_CLONE_NOTEBOOK,
        "Clone Notebook...",
        true,
        None::<&str>,
    )?)?;
    menu.append(&file_menu)?;

    // Edit menu (standard text editing)
    let edit_menu = Submenu::new(app, "Edit", true)?;
    edit_menu.append(&PredefinedMenuItem::undo(app, None)?)?;
    edit_menu.append(&PredefinedMenuItem::redo(app, None)?)?;
    edit_menu.append(&PredefinedMenuItem::separator(app)?)?;
    edit_menu.append(&PredefinedMenuItem::cut(app, None)?)?;
    edit_menu.append(&PredefinedMenuItem::copy(app, None)?)?;
    edit_menu.append(&PredefinedMenuItem::paste(app, None)?)?;
    edit_menu.append(&PredefinedMenuItem::select_all(app, None)?)?;
    menu.append(&edit_menu)?;

    // Cell menu
    let cell_menu = Submenu::new(app, "Cell", true)?;
    cell_menu.append(&MenuItem::with_id(
        app,
        MENU_INSERT_CODE_CELL,
        "Insert Code Cell",
        true,
        Some("CmdOrCtrl+Shift+C"),
    )?)?;
    cell_menu.append(&MenuItem::with_id(
        app,
        MENU_INSERT_MARKDOWN_CELL,
        "Insert Markdown Cell",
        true,
        Some("CmdOrCtrl+Shift+M"),
    )?)?;
    cell_menu.append(&MenuItem::with_id(
        app,
        MENU_INSERT_RAW_CELL,
        "Insert Raw Cell",
        true,
        Some("CmdOrCtrl+Shift+R"),
    )?)?;
    cell_menu.append(&PredefinedMenuItem::separator(app)?)?;
    cell_menu.append(&MenuItem::with_id(
        app,
        MENU_CLEAR_OUTPUTS,
        "Clear Outputs",
        true,
        None::<&str>,
    )?)?;
    cell_menu.append(&MenuItem::with_id(
        app,
        MENU_CLEAR_ALL_OUTPUTS,
        "Clear All Outputs",
        true,
        None::<&str>,
    )?)?;
    menu.append(&cell_menu)?;

    // Runtime menu
    let kernel_menu = Submenu::new(app, "Runtime", true)?;
    kernel_menu.append(&MenuItem::with_id(
        app,
        MENU_RUN_ALL_CELLS,
        "Run All Cells",
        true,
        None::<&str>,
    )?)?;
    kernel_menu.append(&MenuItem::with_id(
        app,
        MENU_RESTART_AND_RUN_ALL,
        "Restart & Run All Cells",
        true,
        None::<&str>,
    )?)?;
    menu.append(&kernel_menu)?;

    // View menu
    let view_menu = Submenu::new(app, "View", true)?;
    view_menu.append(&MenuItem::with_id(
        app,
        MENU_ZOOM_IN,
        "Zoom In",
        true,
        Some("CmdOrCtrl+="),
    )?)?;
    view_menu.append(&MenuItem::with_id(
        app,
        MENU_ZOOM_OUT,
        "Zoom Out",
        true,
        Some("CmdOrCtrl+-"),
    )?)?;
    view_menu.append(&MenuItem::with_id(
        app,
        MENU_ZOOM_RESET,
        "Actual Size",
        true,
        Some("CmdOrCtrl+0"),
    )?)?;
    menu.append(&view_menu)?;

    // Window menu
    let window_menu = Submenu::new(app, "Window", true)?;
    window_menu.append(&PredefinedMenuItem::minimize(app, None)?)?;
    window_menu.append(&PredefinedMenuItem::close_window(app, None)?)?;
    let mut window_entries: Vec<_> = app
        .webview_windows()
        .into_keys()
        .map(|window_label| {
            let display_name = window_display_names
                .get(&window_label)
                .cloned()
                .unwrap_or_else(|| window_label.clone());
            (window_label, display_name)
        })
        .collect();
    window_entries.sort_by(|(label_a, title_a), (label_b, title_b)| {
        title_a.cmp(title_b).then_with(|| label_a.cmp(label_b))
    });
    if !window_entries.is_empty() {
        window_menu.append(&PredefinedMenuItem::separator(app)?)?;
        for (window_label, display_name) in window_entries {
            window_menu.append(&MenuItem::with_id(
                app,
                window_menu_item_id(&window_label),
                display_name,
                true,
                None::<&str>,
            )?)?;
        }
    }
    menu.append(&window_menu)?;

    Ok(menu)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        about_menu_label, app_name, build_about_metadata, sample_for_menu_item_id,
        sample_menu_item_id, window_label_for_menu_item_id, window_menu_item_id, APP_COMMIT_SHA,
        APP_RELEASE_DATE, APP_VERSION, BUNDLED_SAMPLE_NOTEBOOKS,
    };
    use std::collections::HashSet;

    #[test]
    fn bundled_sample_ids_are_unique() {
        let mut ids = HashSet::new();
        for sample in BUNDLED_SAMPLE_NOTEBOOKS {
            assert!(ids.insert(sample.id), "duplicate sample id: {}", sample.id);
        }
    }

    #[test]
    fn bundled_sample_file_names_are_unique() {
        let mut names = HashSet::new();
        for sample in BUNDLED_SAMPLE_NOTEBOOKS {
            assert!(
                names.insert(sample.file_name),
                "duplicate sample file name: {}",
                sample.file_name
            );
            assert!(sample.file_name.ends_with(".ipynb"));
        }
    }

    #[test]
    fn sample_menu_ids_round_trip() {
        for sample in BUNDLED_SAMPLE_NOTEBOOKS {
            let menu_id = sample_menu_item_id(sample.id);
            let resolved = sample_for_menu_item_id(&menu_id).expect("sample should resolve");
            assert_eq!(resolved.id, sample.id);
        }
    }

    #[test]
    fn window_menu_ids_round_trip() {
        for label in ["main", "onboarding", "notebook-123"] {
            let menu_id = window_menu_item_id(label);
            let resolved = window_label_for_menu_item_id(&menu_id).expect("window should resolve");
            assert_eq!(resolved, label);
        }
        assert!(window_label_for_menu_item_id("new_notebook").is_none());
    }

    #[test]
    fn bundled_samples_are_valid_notebooks() {
        for sample in BUNDLED_SAMPLE_NOTEBOOKS {
            nbformat::parse_notebook(sample.contents)
                .unwrap_or_else(|e| panic!("{} should parse: {}", sample.file_name, e));
        }
    }

    #[test]
    fn about_menu_label_matches_app_name() {
        assert_eq!(about_menu_label(), format!("About {}", app_name()));
    }

    #[test]
    fn about_metadata_includes_required_release_fields() {
        let metadata = build_about_metadata();
        assert_eq!(metadata.name.as_deref(), Some(app_name()));
        assert_eq!(metadata.version.as_deref(), Some(APP_VERSION));
        let comments = metadata
            .comments
            .as_deref()
            .expect("about metadata should include comments");
        assert!(
            comments.contains(APP_COMMIT_SHA),
            "comments should include commit SHA"
        );
        assert!(
            comments.contains(APP_RELEASE_DATE),
            "comments should include release date"
        );
    }
}
