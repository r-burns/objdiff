use std::{
    path::{Path, PathBuf},
    process::Command,
    str::from_utf8,
    sync::mpsc::Receiver,
};

use anyhow::{anyhow, Context, Error, Result};
use objdiff_core::{
    diff::{diff_objs, DiffAlg, DiffObjConfig},
    obj::{elf, ObjInfo},
};
use time::OffsetDateTime;

use crate::{
    app::{AppConfig, ObjectConfig},
    jobs::{start_job, update_status, Job, JobContext, JobResult, JobState},
};

pub struct BuildStatus {
    pub success: bool,
    pub cmdline: String,
    pub stdout: String,
    pub stderr: String,
}

impl Default for BuildStatus {
    fn default() -> Self {
        BuildStatus {
            success: true,
            cmdline: String::new(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub project_dir: Option<PathBuf>,
    pub custom_make: Option<String>,
    pub selected_wsl_distro: Option<String>,
}

impl BuildConfig {
    pub(crate) fn from_config(config: &AppConfig) -> Self {
        Self {
            project_dir: config.project_dir.clone(),
            custom_make: config.custom_make.clone(),
            selected_wsl_distro: config.selected_wsl_distro.clone(),
        }
    }
}

pub struct ObjDiffConfig {
    pub build_config: BuildConfig,
    pub build_base: bool,
    pub build_target: bool,
    pub selected_obj: Option<ObjectConfig>,
    pub code_alg: DiffAlg,
    pub data_alg: DiffAlg,
    pub relax_reloc_diffs: bool,
}

impl ObjDiffConfig {
    pub(crate) fn from_config(config: &AppConfig) -> Self {
        Self {
            build_config: BuildConfig::from_config(config),
            build_base: config.build_base,
            build_target: config.build_target,
            selected_obj: config.selected_obj.clone(),
            code_alg: config.code_alg,
            data_alg: config.data_alg,
            relax_reloc_diffs: config.relax_reloc_diffs,
        }
    }
}

pub struct ObjDiffResult {
    pub first_status: BuildStatus,
    pub second_status: BuildStatus,
    pub first_obj: Option<ObjInfo>,
    pub second_obj: Option<ObjInfo>,
    pub time: OffsetDateTime,
}

pub(crate) fn run_make(config: &BuildConfig, arg: &Path) -> BuildStatus {
    let Some(cwd) = &config.project_dir else {
        return BuildStatus {
            success: false,
            stderr: "Missing project dir".to_string(),
            ..Default::default()
        };
    };
    match run_make_cmd(config, cwd, arg) {
        Ok(status) => status,
        Err(e) => BuildStatus { success: false, stderr: e.to_string(), ..Default::default() },
    }
}

fn run_make_cmd(config: &BuildConfig, cwd: &Path, arg: &Path) -> Result<BuildStatus> {
    let make = config.custom_make.as_deref().unwrap_or("make");
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(make);
        command.current_dir(cwd).arg(arg);
        command
    };
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;

        use path_slash::PathExt;
        let mut command = if config.selected_wsl_distro.is_some() {
            Command::new("wsl")
        } else {
            Command::new(make)
        };
        if let Some(distro) = &config.selected_wsl_distro {
            command
                .arg("--cd")
                .arg(cwd)
                .arg("-d")
                .arg(distro)
                .arg("--")
                .arg(make)
                .arg(arg.to_slash_lossy().as_ref());
        } else {
            command.current_dir(cwd).arg(arg.to_slash_lossy().as_ref());
        }
        command.creation_flags(winapi::um::winbase::CREATE_NO_WINDOW);
        command
    };
    let mut cmdline = shell_escape::escape(command.get_program().to_string_lossy()).into_owned();
    for arg in command.get_args() {
        cmdline.push(' ');
        cmdline.push_str(shell_escape::escape(arg.to_string_lossy()).as_ref());
    }
    let output = command.output().context("Failed to execute build")?;
    let stdout = from_utf8(&output.stdout).context("Failed to process stdout")?;
    let stderr = from_utf8(&output.stderr).context("Failed to process stderr")?;
    Ok(BuildStatus {
        success: output.status.code().unwrap_or(-1) == 0,
        cmdline,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    })
}

fn run_build(
    context: &JobContext,
    cancel: Receiver<()>,
    config: ObjDiffConfig,
) -> Result<Box<ObjDiffResult>> {
    let obj_config = config.selected_obj.as_ref().ok_or_else(|| Error::msg("Missing obj path"))?;
    let project_dir = config
        .build_config
        .project_dir
        .as_ref()
        .ok_or_else(|| Error::msg("Missing project dir"))?;
    let target_path_rel = if let Some(target_path) = &obj_config.target_path {
        Some(target_path.strip_prefix(project_dir).map_err(|_| {
            anyhow!(
                "Target path '{}' doesn't begin with '{}'",
                target_path.display(),
                project_dir.display()
            )
        })?)
    } else {
        None
    };
    let base_path_rel = if let Some(base_path) = &obj_config.base_path {
        Some(base_path.strip_prefix(project_dir).map_err(|_| {
            anyhow!(
                "Base path '{}' doesn't begin with '{}'",
                base_path.display(),
                project_dir.display()
            )
        })?)
    } else {
        None
    };

    let mut total = 3;
    if config.build_target && target_path_rel.is_some() {
        total += 1;
    }
    if config.build_base && base_path_rel.is_some() {
        total += 1;
    }
    let first_status = match target_path_rel {
        Some(target_path_rel) if config.build_target => {
            update_status(
                context,
                format!("Building target {}", target_path_rel.display()),
                0,
                total,
                &cancel,
            )?;
            run_make(&config.build_config, target_path_rel)
        }
        _ => BuildStatus::default(),
    };

    let second_status = match base_path_rel {
        Some(base_path_rel) if config.build_base => {
            update_status(
                context,
                format!("Building base {}", base_path_rel.display()),
                0,
                total,
                &cancel,
            )?;
            run_make(&config.build_config, base_path_rel)
        }
        _ => BuildStatus::default(),
    };

    let time = OffsetDateTime::now_utc();

    let mut first_obj =
        match &obj_config.target_path {
            Some(target_path) if first_status.success => {
                update_status(
                    context,
                    format!("Loading target {}", target_path_rel.unwrap().display()),
                    2,
                    total,
                    &cancel,
                )?;
                Some(elf::read(target_path).with_context(|| {
                    format!("Failed to read object '{}'", target_path.display())
                })?)
            }
            _ => None,
        };

    let mut second_obj = match &obj_config.base_path {
        Some(base_path) if second_status.success => {
            update_status(
                context,
                format!("Loading base {}", base_path_rel.unwrap().display()),
                3,
                total,
                &cancel,
            )?;
            Some(
                elf::read(base_path)
                    .with_context(|| format!("Failed to read object '{}'", base_path.display()))?,
            )
        }
        _ => None,
    };

    update_status(context, "Performing diff".to_string(), 4, total, &cancel)?;
    let diff_config = DiffObjConfig {
        code_alg: config.code_alg,
        data_alg: config.data_alg,
        relax_reloc_diffs: config.relax_reloc_diffs,
    };
    diff_objs(&diff_config, first_obj.as_mut(), second_obj.as_mut())?;

    update_status(context, "Complete".to_string(), total, total, &cancel)?;
    Ok(Box::new(ObjDiffResult { first_status, second_status, first_obj, second_obj, time }))
}

pub fn start_build(ctx: &egui::Context, config: ObjDiffConfig) -> JobState {
    start_job(ctx, "Object diff", Job::ObjDiff, move |context, cancel| {
        run_build(&context, cancel, config).map(|result| JobResult::ObjDiff(Some(result)))
    })
}
