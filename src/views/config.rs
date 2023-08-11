#[cfg(windows)]
use std::string::FromUtf16Error;
use std::{
    borrow::Cow,
    path::{PathBuf, MAIN_SEPARATOR},
    sync::{Arc, RwLock},
};

#[cfg(windows)]
use anyhow::{Context, Result};
use const_format::formatcp;
use egui::{
    output::OpenUrl, text::LayoutJob, CollapsingHeader, FontFamily, FontId, RichText,
    SelectableLabel, TextFormat, Widget,
};
use globset::Glob;
use self_update::cargo_crate_version;

use crate::{
    app::AppConfig,
    config::{ProjectUnit, ProjectUnitNode},
    jobs::{check_update::CheckUpdateResult, objdiff::start_build, update::start_update, JobQueue},
    update::RELEASE_URL,
    views::appearance::Appearance,
};

#[derive(Default)]
pub struct ConfigViewState {
    pub check_update: Option<Box<CheckUpdateResult>>,
    pub watch_pattern_text: String,
    pub queue_update_check: bool,
    pub load_error: Option<String>,
    pub unit_search: String,
    #[cfg(windows)]
    pub available_wsl_distros: Option<Vec<String>>,
}

const DEFAULT_WATCH_PATTERNS: &[&str] = &[
    "*.c", "*.cp", "*.cpp", "*.cxx", "*.h", "*.hp", "*.hpp", "*.hxx", "*.s", "*.S", "*.asm",
    "*.inc", "*.py", "*.yml", "*.txt", "*.json",
];

#[cfg(windows)]
fn process_utf16(bytes: &[u8]) -> Result<String, FromUtf16Error> {
    let u16_bytes: Vec<u16> = bytes
        .chunks_exact(2)
        .filter_map(|c| Some(u16::from_ne_bytes(c.try_into().ok()?)))
        .collect();
    String::from_utf16(&u16_bytes)
}

#[cfg(windows)]
fn wsl_cmd(args: &[&str]) -> Result<String> {
    use std::{os::windows::process::CommandExt, process::Command};
    let output = Command::new("wsl")
        .args(args)
        .creation_flags(winapi::um::winbase::CREATE_NO_WINDOW)
        .output()
        .context("Failed to execute wsl")?;
    process_utf16(&output.stdout).context("Failed to process stdout")
}

#[cfg(windows)]
fn fetch_wsl2_distros() -> Vec<String> {
    wsl_cmd(&["-l", "-q"])
        .map(|stdout| {
            stdout
                .split('\n')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub fn config_ui(
    ui: &mut egui::Ui,
    config: &Arc<RwLock<AppConfig>>,
    jobs: &mut JobQueue,
    show_config_window: &mut bool,
    state: &mut ConfigViewState,
    appearance: &Appearance,
) {
    let mut config_guard = config.write().unwrap();
    let AppConfig {
        selected_wsl_distro,
        target_obj_dir,
        base_obj_dir,
        obj_path,
        auto_update_check,
        units,
        unit_nodes,
        ..
    } = &mut *config_guard;

    ui.heading("Updates");
    ui.checkbox(auto_update_check, "Check for updates on startup");
    if ui.button("Check now").clicked() {
        state.queue_update_check = true;
    }
    ui.label(format!("Current version: {}", cargo_crate_version!())).on_hover_ui_at_pointer(|ui| {
        ui.label(formatcp!("Git branch: {}", env!("VERGEN_GIT_BRANCH")));
        ui.label(formatcp!("Git commit: {}", env!("VERGEN_GIT_SHA")));
        ui.label(formatcp!("Build target: {}", env!("VERGEN_CARGO_TARGET_TRIPLE")));
        ui.label(formatcp!("Debug: {}", env!("VERGEN_CARGO_DEBUG")));
    });
    if let Some(state) = &state.check_update {
        ui.label(format!("Latest version: {}", state.latest_release.version));
        if state.update_available {
            ui.colored_label(appearance.insert_color, "Update available");
            ui.horizontal(|ui| {
                if state.found_binary
                    && ui
                        .button("Automatic")
                        .on_hover_text_at_pointer(
                            "Automatically download and replace the current build",
                        )
                        .clicked()
                {
                    jobs.push(start_update());
                }
                if ui
                    .button("Manual")
                    .on_hover_text_at_pointer("Open a link to the latest release on GitHub")
                    .clicked()
                {
                    ui.output_mut(|output| {
                        output.open_url =
                            Some(OpenUrl { url: RELEASE_URL.to_string(), new_tab: true })
                    });
                }
            });
        }
    }
    ui.separator();

    #[cfg(windows)]
    {
        ui.heading("Build");
        if state.available_wsl_distros.is_none() {
            state.available_wsl_distros = Some(fetch_wsl2_distros());
        }
        egui::ComboBox::from_label("Run in WSL2")
            .selected_text(selected_wsl_distro.as_ref().unwrap_or(&"Disabled".to_string()))
            .show_ui(ui, |ui| {
                ui.selectable_value(selected_wsl_distro, None, "Disabled");
                for distro in state.available_wsl_distros.as_ref().unwrap() {
                    ui.selectable_value(selected_wsl_distro, Some(distro.clone()), distro);
                }
            });
        ui.separator();
    }
    #[cfg(not(windows))]
    {
        let _ = selected_wsl_distro;
    }

    ui.horizontal(|ui| {
        ui.heading("Project");
        if ui.button(RichText::new("Settings")).clicked() {
            *show_config_window = true;
        }
    });

    if let (Some(base_dir), Some(target_dir)) = (base_obj_dir, target_obj_dir) {
        let mut new_build_obj = obj_path.clone();
        if units.is_empty() {
            if ui.button("Select object").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_directory(&target_dir)
                    .add_filter("Object file", &["o", "elf"])
                    .pick_file()
                {
                    if let Ok(obj_path) = path.strip_prefix(&base_dir) {
                        new_build_obj = Some(obj_path.display().to_string());
                    } else if let Ok(obj_path) = path.strip_prefix(&target_dir) {
                        new_build_obj = Some(obj_path.display().to_string());
                    }
                }
            }
            if let Some(obj) = obj_path {
                ui.label(
                    RichText::new(&*obj)
                        .color(appearance.replace_color)
                        .family(FontFamily::Monospace),
                );
            }
        } else {
            let had_search = !state.unit_search.is_empty();
            egui::TextEdit::singleline(&mut state.unit_search).hint_text("Filter").ui(ui);

            let mut root_open = None;
            let mut node_open = NodeOpen::Default;
            ui.horizontal(|ui| {
                if ui.small_button("⏶").on_hover_text_at_pointer("Collapse all").clicked() {
                    root_open = Some(false);
                    node_open = NodeOpen::Close;
                }
                if ui.small_button("⏷").on_hover_text_at_pointer("Expand all").clicked() {
                    root_open = Some(true);
                    node_open = NodeOpen::Open;
                }
                if ui
                    .add_enabled(obj_path.is_some(), egui::Button::new("⌖").small())
                    .on_hover_text_at_pointer("Current object")
                    .clicked()
                {
                    root_open = Some(true);
                    node_open = NodeOpen::Object;
                }
            });
            if state.unit_search.is_empty() {
                if had_search {
                    root_open = Some(true);
                    node_open = NodeOpen::Object;
                }
            } else if !had_search {
                root_open = Some(true);
                node_open = NodeOpen::Open;
            }

            CollapsingHeader::new(RichText::new("🗀 Objects").font(FontId {
                size: appearance.ui_font.size,
                family: appearance.code_font.family.clone(),
            }))
            .open(root_open)
            .default_open(true)
            .show(ui, |ui| {
                let mut nodes = Cow::Borrowed(unit_nodes);
                if !state.unit_search.is_empty() {
                    let search = state.unit_search.to_ascii_lowercase();
                    nodes = Cow::Owned(
                        unit_nodes.iter().filter_map(|node| filter_node(node, &search)).collect(),
                    );
                }

                ui.style_mut().wrap = Some(false);
                for node in nodes.iter() {
                    display_node(ui, &mut new_build_obj, node, appearance, node_open);
                }
            });
        }

        if new_build_obj != *obj_path {
            if let Some(obj) = new_build_obj {
                // Will set obj_changed, which will trigger a rebuild
                config_guard.set_obj_path(obj);
                // TODO apply reverse_fn_order
            }
        }
        if config_guard.obj_path.is_some() && ui.button("Build").clicked() {
            // Rebuild immediately
            jobs.push(start_build(config.clone()));
        }
    } else {
        ui.colored_label(appearance.delete_color, "Missing project settings");
    }

    // ui.checkbox(&mut view_config.reverse_fn_order, "Reverse function order (deferred)");
    ui.separator();
}

fn display_unit(
    ui: &mut egui::Ui,
    obj_path: &mut Option<String>,
    name: &str,
    unit: &ProjectUnit,
    appearance: &Appearance,
) {
    let path_string = unit.path.to_string_lossy().to_string();
    let selected = matches!(obj_path, Some(path) if path == &path_string);
    if SelectableLabel::new(
        selected,
        RichText::new(name)
            .font(FontId {
                size: appearance.ui_font.size,
                family: appearance.code_font.family.clone(),
            })
            .color(appearance.text_color),
    )
    .ui(ui)
    .clicked()
    {
        *obj_path = Some(path_string);
    }
}

#[derive(Default, Copy, Clone, PartialEq, Eq, Debug)]
enum NodeOpen {
    #[default]
    Default,
    Open,
    Close,
    Object,
}

fn display_node(
    ui: &mut egui::Ui,
    obj_path: &mut Option<String>,
    node: &ProjectUnitNode,
    appearance: &Appearance,
    node_open: NodeOpen,
) {
    match node {
        ProjectUnitNode::File(name, unit) => {
            display_unit(ui, obj_path, name, unit, appearance);
        }
        ProjectUnitNode::Dir(name, children) => {
            let contains_obj = obj_path.as_ref().map(|path| contains_node(node, path));
            let open = match node_open {
                NodeOpen::Default => None,
                NodeOpen::Open => Some(true),
                NodeOpen::Close => Some(false),
                NodeOpen::Object => contains_obj,
            };
            let color = if contains_obj == Some(true) {
                appearance.replace_color
            } else {
                appearance.text_color
            };
            CollapsingHeader::new(
                RichText::new(name)
                    .font(FontId {
                        size: appearance.ui_font.size,
                        family: appearance.code_font.family.clone(),
                    })
                    .color(color),
            )
            .open(open)
            .show(ui, |ui| {
                for node in children {
                    display_node(ui, obj_path, node, appearance, node_open);
                }
            });
        }
    }
}

fn contains_node(node: &ProjectUnitNode, path: &str) -> bool {
    match node {
        ProjectUnitNode::File(_, unit) => {
            let path_string = unit.path.to_string_lossy().to_string();
            path == path_string
        }
        ProjectUnitNode::Dir(_, children) => children.iter().any(|node| contains_node(node, path)),
    }
}

fn filter_node(node: &ProjectUnitNode, search: &str) -> Option<ProjectUnitNode> {
    match node {
        ProjectUnitNode::File(name, _) => {
            if name.to_ascii_lowercase().contains(search) {
                Some(node.clone())
            } else {
                None
            }
        }
        ProjectUnitNode::Dir(name, children) => {
            if name.to_ascii_lowercase().contains(search) {
                return Some(node.clone());
            }
            let new_children =
                children.iter().filter_map(|child| filter_node(child, search)).collect::<Vec<_>>();
            if !new_children.is_empty() {
                Some(ProjectUnitNode::Dir(name.clone(), new_children))
            } else {
                None
            }
        }
    }
}

const HELP_ICON: &str = "ℹ";

fn subheading(ui: &mut egui::Ui, text: &str, appearance: &Appearance) {
    ui.label(
        RichText::new(text).size(appearance.ui_font.size).color(appearance.emphasized_text_color),
    );
}

fn format_path(path: &Option<PathBuf>, appearance: &Appearance) -> RichText {
    let mut color = appearance.replace_color;
    let text = if let Some(dir) = path {
        if let Some(rel) = dirs::home_dir().and_then(|home| dir.strip_prefix(&home).ok()) {
            format!("~{}{}", MAIN_SEPARATOR, rel.display())
        } else {
            format!("{}", dir.display())
        }
    } else {
        color = appearance.delete_color;
        "[none]".to_string()
    };
    RichText::new(text).color(color).family(FontFamily::Monospace)
}

fn pick_folder_ui(
    ui: &mut egui::Ui,
    dir: &Option<PathBuf>,
    label: &str,
    tooltip: impl FnOnce(&mut egui::Ui),
    appearance: &Appearance,
) -> egui::Response {
    let response = ui.horizontal(|ui| {
        subheading(ui, label, appearance);
        ui.link(HELP_ICON).on_hover_ui(tooltip);
        ui.button("Select")
    });
    ui.label(format_path(dir, appearance));
    response.inner
}

pub fn project_window(
    ctx: &egui::Context,
    config: &Arc<RwLock<AppConfig>>,
    show: &mut bool,
    state: &mut ConfigViewState,
    appearance: &Appearance,
) {
    let mut config_guard = config.write().unwrap();

    egui::Window::new("Project").open(show).show(ctx, |ui| {
        split_obj_config_ui(ui, &mut config_guard, state, appearance);
    });

    if let Some(error) = &state.load_error {
        let mut open = true;
        egui::Window::new("Error").open(&mut open).show(ctx, |ui| {
            ui.label("Failed to load project config:");
            ui.colored_label(appearance.delete_color, error);
        });
        if !open {
            state.load_error = None;
        }
    }
}

fn split_obj_config_ui(
    ui: &mut egui::Ui,
    config: &mut AppConfig,
    state: &mut ConfigViewState,
    appearance: &Appearance,
) {
    let text_format = TextFormat::simple(appearance.ui_font.clone(), appearance.text_color);
    let code_format = TextFormat::simple(
        FontId { size: appearance.ui_font.size, family: appearance.code_font.family.clone() },
        appearance.emphasized_text_color,
    );

    let response = pick_folder_ui(
        ui,
        &config.project_dir,
        "Project directory",
        |ui| {
            let mut job = LayoutJob::default();
            job.append("The root project directory.\n\n", 0.0, text_format.clone());
            job.append(
                "If a configuration file exists, it will be loaded automatically.",
                0.0,
                text_format.clone(),
            );
            ui.label(job);
        },
        appearance,
    );
    if response.clicked() {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            config.set_project_dir(path);
        }
    }
    ui.separator();

    ui.horizontal(|ui| {
        subheading(ui, "Custom make program", appearance);
        ui.link(HELP_ICON).on_hover_ui(|ui| {
            let mut job = LayoutJob::default();
            job.append("By default, objdiff will build with ", 0.0, text_format.clone());
            job.append("make", 0.0, code_format.clone());
            job.append(
                ".\nIf the project uses a different build system (e.g. ",
                0.0,
                text_format.clone(),
            );
            job.append("ninja", 0.0, code_format.clone());
            job.append(
                "), specify it here.\nThe program must be in your ",
                0.0,
                text_format.clone(),
            );
            job.append("PATH", 0.0, code_format.clone());
            job.append(".", 0.0, text_format.clone());
            ui.label(job);
        });
    });
    let mut custom_make_str = config.custom_make.clone().unwrap_or_default();
    if ui.text_edit_singleline(&mut custom_make_str).changed() {
        if custom_make_str.is_empty() {
            config.custom_make = None;
        } else {
            config.custom_make = Some(custom_make_str);
        }
    }
    ui.separator();

    if let Some(project_dir) = config.project_dir.clone() {
        let response = pick_folder_ui(
            ui,
            &config.target_obj_dir,
            "Target build directory",
            |ui| {
                let mut job = LayoutJob::default();
                job.append(
                    "This contains the \"target\" or \"expected\" objects, which are the intended result of the match.\n\n",
                    0.0,
                    text_format.clone(),
                );
                job.append(
                    "These are usually created by the project's build system or assembled.",
                    0.0,
                    text_format.clone(),
                );
                ui.label(job);
            },
            appearance,
        );
        if response.clicked() {
            if let Some(path) = rfd::FileDialog::new().set_directory(&project_dir).pick_folder() {
                config.set_target_obj_dir(path);
            }
        }
        ui.checkbox(&mut config.build_target, "Build target objects").on_hover_ui(|ui| {
            let mut job = LayoutJob::default();
            job.append(
                "Tells the build system to produce the target object.\n",
                0.0,
                text_format.clone(),
            );
            job.append("For example, this would call ", 0.0, text_format.clone());
            job.append("make path/to/target.o", 0.0, code_format.clone());
            job.append(".\n\n", 0.0, text_format.clone());
            job.append(
                "This is useful if the target objects are not already built\n",
                0.0,
                text_format.clone(),
            );
            job.append(
                "or if they can change based on project configuration,\n",
                0.0,
                text_format.clone(),
            );
            job.append(
                "but requires that the build system is configured correctly.",
                0.0,
                text_format.clone(),
            );
            ui.label(job);
        });
        ui.separator();

        let response = pick_folder_ui(
            ui,
            &config.base_obj_dir,
            "Base build directory",
            |ui| {
                let mut job = LayoutJob::default();
                job.append(
                    "This contains the objects built from your decompiled code.",
                    0.0,
                    text_format.clone(),
                );
                ui.label(job);
            },
            appearance,
        );
        if response.clicked() {
            if let Some(path) = rfd::FileDialog::new().set_directory(&project_dir).pick_folder() {
                config.set_base_obj_dir(path);
            }
        }
        ui.separator();
    }

    subheading(ui, "Watch settings", appearance);
    let response =
        ui.checkbox(&mut config.watcher_enabled, "Rebuild on changes").on_hover_ui(|ui| {
            let mut job = LayoutJob::default();
            job.append(
                "Automatically re-run the build & diff when files change.",
                0.0,
                text_format.clone(),
            );
            ui.label(job);
        });
    if response.changed() {
        config.watcher_change = true;
    };

    ui.horizontal(|ui| {
        ui.label(RichText::new("File patterns").color(appearance.text_color));
        if ui.button("Reset").clicked() {
            config.watch_patterns =
                DEFAULT_WATCH_PATTERNS.iter().map(|s| Glob::new(s).unwrap()).collect();
            config.watcher_change = true;
        }
    });
    let mut remove_at: Option<usize> = None;
    for (idx, glob) in config.watch_patterns.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{}", glob))
                    .color(appearance.text_color)
                    .family(FontFamily::Monospace),
            );
            if ui.small_button("-").clicked() {
                remove_at = Some(idx);
            }
        });
    }
    if let Some(idx) = remove_at {
        config.watch_patterns.remove(idx);
        config.watcher_change = true;
    }
    ui.horizontal(|ui| {
        egui::TextEdit::singleline(&mut state.watch_pattern_text).desired_width(100.0).show(ui);
        if ui.small_button("+").clicked() {
            if let Ok(glob) = Glob::new(&state.watch_pattern_text) {
                config.watch_patterns.push(glob);
                config.watcher_change = true;
                state.watch_pattern_text.clear();
            }
        }
    });
}
